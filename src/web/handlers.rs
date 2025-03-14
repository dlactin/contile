//! API Handlers
use actix_web::{web, HttpRequest, HttpResponse};
use actix_web_location::Location;
use lazy_static::lazy_static;
use rand::{thread_rng, Rng};

use crate::{
    adm,
    error::{HandlerErrorKind, HandlerResult},
    metrics::Metrics,
    server::{
        cache::{self, Tiles, TilesState},
        ServerState,
    },
    settings::Settings,
    tags::Tags,
    web::{middleware::sentry as l_sentry, DeviceInfo},
};

lazy_static! {
    static ref EMPTY_TILES: String = serde_json::to_string(&adm::TileResponse { tiles: vec![] })
        .expect("Couldn't serialize EMPTY_TILES");
}

/// Calculate the ttl from the settings by taking the tiles_ttl
/// and calculating a jitter that is no more than 50% of the total TTL.
/// It is recommended that "jitter" be 10%.
pub fn add_jitter(settings: &Settings) -> u32 {
    let mut rng = thread_rng();
    let ftl = settings.tiles_ttl as f32;
    let offset = ftl * (std::cmp::min(settings.jitter, 50) as f32 * 0.01);
    let jit = rng.gen_range(0.0 - offset..offset);
    (ftl + jit) as u32
}

/// Handler for `.../v1/tiles` endpoint
///
/// Normalizes User Agent info and searches cache for possible tile suggestions.
/// On a miss, it will attempt to fetch new tiles from ADM.
pub async fn get_tiles(
    location: Location,
    device_info: DeviceInfo,
    metrics: Metrics,
    state: web::Data<ServerState>,
    request: HttpRequest,
) -> HandlerResult<HttpResponse> {
    trace!("get_tiles");
    metrics.incr("tiles.get");

    let settings = &state.settings;
    if !state
        .filter
        .read()
        .unwrap()
        .all_include_regions
        .contains(&location.country())
    {
        trace!("get_tiles: country not included: {:?}", location.country());
        // Nothing to serve. We typically send a 204 for empty tiles but
        // optionally send 200 to resolve
        // https://github.com/mozilla-services/contile/issues/284
        let response = if settings.excluded_countries_200 {
            HttpResponse::Ok()
                .content_type("application/json")
                .body(&*EMPTY_TILES)
        } else {
            HttpResponse::NoContent().finish()
        };
        return Ok(response);
    }

    let audience_key = cache::AudienceKey {
        country_code: location.country(),
        region_code: if location.region() != "" {
            Some(location.region())
        } else {
            None
        },
        dma_code: location.dma,
        form_factor: device_info.form_factor,
        os_family: device_info.os_family,
        legacy_only: device_info.legacy_only(),
    };

    let mut tags = Tags::default();
    {
        tags.add_extra("audience_key", &format!("{:#?}", audience_key));
        // Add/modify the existing request tags.
        // tags.clone().commit(&mut request.extensions_mut());
    }

    let mut expired = false;
    if settings.test_mode != crate::settings::TestModes::TestFakeResponse {
        // First make a cheap read from the cache
        if let Some(tiles_state) = state.tiles_cache.get(&audience_key) {
            match &*tiles_state {
                TilesState::Populating => {
                    // Another task is currently populating this entry and will
                    // complete shortly. 204 until then instead of queueing
                    // more redundant requests
                    trace!("get_tiles: Another task Populating");
                    metrics.incr("tiles_cache.miss.populating");
                    return Ok(HttpResponse::NoContent().finish());
                }
                TilesState::Fresh { tiles } => {
                    expired = tiles.expired();
                    if !expired {
                        trace!("get_tiles: cache hit: {:?}", audience_key);
                        metrics.incr("tiles_cache.hit");
                        return Ok(content_response(&tiles.content));
                    }
                    // Needs refreshing
                }
                TilesState::Refreshing { tiles } => {
                    // Another task is currently refreshing this entry, just
                    // return the stale Tiles until it's completed
                    trace!(
                        "get_tiles: cache hit (expired, Refreshing): {:?}",
                        audience_key
                    );
                    metrics.incr("tiles_cache.hit.refreshing");
                    return Ok(content_response(&tiles.content));
                }
            }
        }
    }

    // Alter the cache separately from the read above: writes are more
    // expensive and these alterations occur infrequently

    // Prepare to write: temporarily set the cache entry to
    // Refreshing/Populating until we've completed our write, notifying other
    // requests in flight during this time to return stale data/204 No Content
    // instead of making duplicate/redundant writes. The handle will reset the
    // temporary state if no write occurs (due to errors/panics)
    let handle = state.tiles_cache.prepare_write(&audience_key, expired);

    let result = adm::get_tiles(
        &state,
        &location,
        device_info,
        &mut tags,
        &metrics,
        // be aggressive about not passing headers unless we absolutely need to
        if settings.test_mode != crate::settings::TestModes::NoTest {
            Some(request.head().headers())
        } else {
            None
        },
    )
    .await;

    match result {
        Ok(response) => {
            let tiles = cache::Tiles::new(response, add_jitter(&state.settings))?;
            trace!(
                "get_tiles: cache miss{}: {:?}",
                if expired { " (expired)" } else { "" },
                &audience_key
            );
            metrics.incr("tiles_cache.miss");
            handle.insert(TilesState::Fresh {
                tiles: tiles.clone(),
            });
            Ok(content_response(&tiles.content))
        }
        Err(e) => {
            // Add some kind of stats to Retrieving or RetrievingFirst?
            // do we need a kill switch if we're restricting like this already?
            match e.kind() {
                HandlerErrorKind::BadAdmResponse(es) => {
                    warn!("Bad response from ADM: {:?}", e);
                    metrics.incr_with_tags("tiles.invalid", Some(&tags));
                    handle.insert(TilesState::Fresh {
                        tiles: Tiles::empty(add_jitter(&state.settings)),
                    });
                    // Report directly to sentry
                    // (This is starting to become a pattern. 🤔)
                    let mut tags = Tags::from_head(request.head(), settings);
                    tags.add_extra("err", es);
                    tags.add_tag("level", "warning");
                    l_sentry::report(sentry::event_from_error(&e), &tags);
                    warn!("ADM Server error: {:?}", e);
                    Ok(HttpResponse::NoContent().finish())
                }
                _ => Err(e),
            }
        }
    }
}

fn content_response(content: &cache::TilesContent) -> HttpResponse {
    match content {
        cache::TilesContent::Json(json) => HttpResponse::Ok()
            .content_type("application/json")
            .body(json),
        cache::TilesContent::Empty => HttpResponse::NoContent().finish(),
    }
}
