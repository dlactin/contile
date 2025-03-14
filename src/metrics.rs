//! Metric collection and reporting via the `cadence` library.

use std::net::UdpSocket;
use std::time::Instant;

use actix_web::{error::ErrorInternalServerError, web::Data, Error, HttpRequest};
use cadence::{
    BufferedUdpMetricSink, Counted, CountedExt, Metric, NopMetricSink, QueuingMetricSink,
    StatsdClient, Timed,
};

use crate::error::{HandlerError, HandlerErrorKind};
use crate::server::ServerState;
use crate::settings::Settings;
use crate::tags::Tags;

/// A convenience wrapper to time a given operation
#[derive(Debug, Clone)]
pub struct MetricTimer {
    pub label: String,
    pub start: Instant,
    pub tags: Tags,
}

/// The metric wrapper
#[derive(Debug, Clone)]
pub struct Metrics {
    client: Option<StatsdClient>,
    tags: Option<Tags>,
    timer: Option<MetricTimer>,
}

/// Record the duration of a given Timer metric, if one has been set.
impl Drop for Metrics {
    fn drop(&mut self) {
        let tags = self.tags.clone().unwrap_or_default();
        if let Some(client) = self.client.as_ref() {
            if let Some(timer) = self.timer.as_ref() {
                let lapse = (Instant::now() - timer.start).as_millis() as u64;
                trace!("⌚ Ending timer at nanos: {:?} : {:?}", &timer.label, lapse; &tags);
                let mut tagged = client.time_with_tags(&timer.label, lapse);
                // Include any "hard coded" tags.
                // tagged = tagged.with_tag("version", env!("CARGO_PKG_VERSION"));
                let tags = timer.tags.tags.clone();
                let keys = tags.keys();
                for tag in keys {
                    tagged = tagged.with_tag(tag, tags.get(tag).unwrap())
                }
                match tagged.try_send() {
                    Err(e) => {
                        // eat the metric, but log the error
                        warn!("⚠️ Metric {} error: {:?} ", &timer.label, e);
                    }
                    Ok(v) => {
                        trace!("⌚ {:?}", v.as_metric_str());
                    }
                }
            }
        }
    }
}

impl From<&HttpRequest> for Metrics {
    fn from(req: &HttpRequest) -> Self {
        let state = req.app_data::<Data<ServerState>>().expect("No State!");
        let exts = req.extensions();
        let def_tags = Tags::from_head(req.head(), &state.settings);
        let tags = exts.get::<Tags>().unwrap_or(&def_tags);
        Metrics {
            client: match req.app_data::<Data<ServerState>>() {
                Some(v) => Some(*v.metrics.clone()),
                None => {
                    warn!("⚠️ metric error: No App State");
                    None
                }
            },
            tags: Some(tags.clone()),
            timer: None,
        }
    }
}

impl From<&StatsdClient> for Metrics {
    fn from(client: &StatsdClient) -> Self {
        Metrics {
            client: Some(client.clone()),
            tags: None,
            timer: None,
        }
    }
}

impl From<&ServerState> for Metrics {
    fn from(state: &ServerState) -> Self {
        Metrics {
            client: Some(*state.metrics.clone()),
            tags: None,
            timer: None,
        }
    }
}

impl Metrics {
    /// No-op metric
    pub fn sink() -> StatsdClient {
        StatsdClient::builder("", NopMetricSink).build()
    }

    pub fn noop() -> Self {
        Self {
            client: Some(Self::sink()),
            timer: None,
            tags: None,
        }
    }

    /// Start a timer for a given closure.
    ///
    /// Duration is calculated when this timer is dropped.
    pub fn start_timer(&mut self, label: &str, tags: Option<Tags>) {
        let mut mtags = self.tags.clone().unwrap_or_default();
        if let Some(t) = tags {
            mtags.extend(t)
        }

        trace!("⌚ Starting timer... {:?}", &label; &mtags);
        self.timer = Some(MetricTimer {
            label: label.to_owned(),
            start: Instant::now(),
            tags: mtags,
        });
    }

    /// Increment a counter with no tags data.
    pub fn incr(&self, label: &str) {
        self.incr_with_tags(label, None)
    }

    /// Increment a given metric with optional [crate::tags::Tags]
    pub fn incr_with_tags(&self, label: &str, tags: Option<&Tags>) {
        if let Some(client) = self.client.as_ref() {
            let mut tagged = client.incr_with_tags(label);
            let mut mtags = self.tags.clone().unwrap_or_default();
            if let Some(tags) = tags {
                mtags.extend(tags.clone());
            }
            for key in mtags.tags.keys().clone() {
                if let Some(val) = mtags.tags.get(key) {
                    tagged = tagged.with_tag(key, val.as_ref());
                }
            }
            // Include any "hard coded" tags.
            // incr = incr.with_tag("version", env!("CARGO_PKG_VERSION"));
            match tagged.try_send() {
                Err(e) => {
                    // eat the metric, but log the error
                    warn!("⚠️ Metric {} error: {:?} ", label, e; mtags);
                }
                Ok(v) => trace!("☑️ {:?}", v.as_metric_str()),
            }
        }
    }

    /// increment by count with no tags
    pub fn count(&self, label: &str, count: i64) {
        self.count_with_tags(label, count, None)
    }

    /// increment by count with [crate::tags::Tags] information
    pub fn count_with_tags(&self, label: &str, count: i64, tags: Option<Tags>) {
        if let Some(client) = self.client.as_ref() {
            let mut tagged = client.count_with_tags(label, count);
            let mut mtags = self.tags.clone().unwrap_or_default();
            if let Some(tags) = tags {
                mtags.extend(tags);
            }
            for key in mtags.tags.keys().clone() {
                if let Some(val) = mtags.tags.get(key) {
                    tagged = tagged.with_tag(key, val.as_ref());
                }
            }
            // mix in the metric only tags.
            for key in mtags.metric.keys().clone() {
                if let Some(val) = mtags.metric.get(key) {
                    tagged = tagged.with_tag(key, val.as_ref())
                }
            }
            // Include any "hard coded" tags.
            // incr = incr.with_tag("version", env!("CARGO_PKG_VERSION"));
            match tagged.try_send() {
                Err(e) => {
                    // eat the metric, but log the error
                    warn!("⚠️ Metric {} error: {:?} ", label, e; mtags);
                }
                Ok(v) => trace!("☑️ {:?}", v.as_metric_str()),
            }
        }
    }
}

/// Fetch the metric information from the current [HttpRequest]
pub fn metrics_from_req(req: &HttpRequest) -> Result<Box<StatsdClient>, Error> {
    Ok(req
        .app_data::<Data<ServerState>>()
        .ok_or_else(|| ErrorInternalServerError("Could not get state"))
        .expect("Could not get state in metrics_from_req")
        .metrics
        .clone())
}

/// Create a cadence StatsdClient from the given options
pub fn metrics_from_opts(opts: &Settings) -> Result<StatsdClient, HandlerError> {
    let builder = if let Some(statsd_host) = opts.statsd_host.as_ref() {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| HandlerErrorKind::Internal(format!("Could not bind UDP port {:?}", e)))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| HandlerErrorKind::Internal(format!("Could not init UDP port {:?}", e)))?;

        let host = (statsd_host.as_str(), opts.statsd_port);
        let udp_sink = BufferedUdpMetricSink::from(host, socket).map_err(|e| {
            HandlerErrorKind::Internal(format!("Could not generate UDP sink {:?}", e))
        })?;
        let sink = QueuingMetricSink::from(udp_sink);
        StatsdClient::builder(opts.statsd_label.as_ref(), sink)
    } else {
        StatsdClient::builder(opts.statsd_label.as_ref(), NopMetricSink)
    };
    Ok(builder
        .with_error_handler(|err| {
            warn!("⚠️ Metric send error:  {:?}", err);
        })
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tags() {
        use actix_web::dev::RequestHead;
        use actix_web::http::{header, uri::Uri};

        let mut rh = RequestHead::default();
        let settings = Settings::default();
        let path = "/1.5/42/storage/meta/global";
        rh.uri = Uri::from_static(path);
        rh.headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:72.0) Gecko/20100101 Firefox/72.0",
            ),
        );

        let tags = Tags::from_head(&rh, &settings);

        assert_eq!(tags.tags.get("ua.os.family"), Some(&"windows".to_owned()));
        assert_eq!(tags.tags.get("ua.form_factor"), Some(&"desktop".to_owned()));
        assert_eq!(tags.tags.get("uri.method"), Some(&"GET".to_owned()));
    }

    #[test]
    fn no_empty_tags() {
        use actix_web::dev::RequestHead;
        use actix_web::http::{header, uri::Uri};

        let mut rh = RequestHead::default();
        let settings = Settings::default();
        let path = "/1.5/42/storage/meta/global";
        rh.uri = Uri::from_static(path);
        rh.headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static("Mozilla/5.0 (curl) Gecko/20100101 curl"),
        );

        let tags = Tags::from_head(&rh, &settings);
        assert!(!tags.tags.contains_key("ua.os.ver"));
        println!("{:?}", tags);
    }
}
