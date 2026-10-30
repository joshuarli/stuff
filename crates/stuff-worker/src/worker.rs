use crate::map::{map_asset, object_key_for, parse_pointer, MappedAsset, WorkerConfig};
use stuff_http::{cache_policy_for, content_type_for, evaluate_preconditions, parse_range, Precondition, RangeError, RouteKind};
use worker::{event, Bucket, Context, Env, Headers, Method, Request, Response, Result};

fn worker_config(env: &Env) -> Result<WorkerConfig> {
    let raw = env.var("STUFF_ROUTES")?.to_string();
    let routes: Vec<RouteEntry> = serde_json::from_str(&raw)
        .map_err(|e| worker::Error::RustError(format!("STUFF_ROUTES: {e}")))?;
    Ok(WorkerConfig {
        routes: routes
            .into_iter()
            .map(|r| crate::map::RegistryRoute {
                url_prefix: r.url_prefix,
                bucket_binding: r.bucket_binding,
                key_prefix: r.key_prefix,
            })
            .collect(),
    })
}

#[derive(serde::Deserialize)]
struct RouteEntry {
    #[serde(default)]
    url_prefix: String,
    bucket_binding: String,
    #[serde(default)]
    key_prefix: String,
}

fn header<'a>(headers: &'a Headers, name: &str) -> Option<String> {
    headers.get(name).ok().flatten()
}

fn file_headers(route_kind: RouteKind, file_name: &str, etag: &str, len: u64) -> Result<Headers> {
    let headers = Headers::new();
    headers.set("content-type", content_type_for(file_name))?;
    headers.set("content-length", &len.to_string())?;
    headers.set("accept-ranges", "bytes")?;
    headers.set("cache-control", &cache_policy_for(route_kind).header_value())?;
    headers.set("etag", etag)?;
    Ok(headers)
}

fn file_name_of(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

async fn serve_object(
    bucket: &Bucket,
    key: &str,
    route_kind: RouteKind,
    method: &Method,
    headers: &Headers,
) -> Result<Response> {
    let object = match bucket.get(key.to_string()).execute().await? {
        Some(o) => o,
        None => return Response::error("not found", 404),
    };
    let size = object.size();
    let etag = object.http_etag();

    match evaluate_preconditions(
        Some(&etag),
        header(headers, "if-none-match").as_deref(),
        header(headers, "if-match").as_deref(),
    ) {
        Precondition::NotModified => {
            let out = Headers::new();
            out.set("etag", &etag)?;
            return Response::empty().map(|r| r.with_status(304).with_headers(out));
        }
        Precondition::Failed => return Response::error("precondition failed", 412),
        Precondition::None => {}
    }

    let range_header = header(headers, "range");
    match range_header.as_deref() {
        Some(spec) => match parse_range(spec, size) {
            Ok(ranges) if ranges.len() == 1 => {
                let r = ranges[0];
                let object = bucket
                    .get(key.to_string())
                    .range(worker::Range::OffsetWithLength {
                        offset: r.start,
                        length: r.len(),
                    })
                    .execute()
                    .await?
                    .ok_or_else(|| worker::Error::RustError("object vanished".to_string()))?;
                let body = object.body().and_then(|b| b.response_body().ok());
                let out = file_headers(route_kind, file_name_of(key), &etag, r.len())?;
                out.set(
                    "content-range",
                    &format!("bytes {}-{}/{}", r.start, r.end_inclusive, size),
                )?;
                match (method, body) {
                    (Method::Head, _) | (_, None) => {
                        Response::empty().map(|r| r.with_status(206).with_headers(out))
                    }
                    (_, Some(body)) => {
                        Response::from_body(body).map(|r| r.with_status(206).with_headers(out))
                    }
                }
            }
            Ok(_) => serve_full(bucket, key, route_kind, method, &etag, size).await,
            Err(RangeError::Unsatisfiable) => {
                let out = Headers::new();
                out.set("content-range", &format!("bytes */{size}"))?;
                out.set("etag", &etag)?;
                Response::empty().map(|r| r.with_status(416).with_headers(out))
            }
            Err(RangeError::Invalid) => serve_full(bucket, key, route_kind, method, &etag, size).await,
        },
        None => serve_full(bucket, key, route_kind, method, &etag, size).await,
    }
}

async fn serve_full(
    bucket: &Bucket,
    key: &str,
    route_kind: RouteKind,
    method: &Method,
    etag: &str,
    size: u64,
) -> Result<Response> {
    let headers = file_headers(route_kind, file_name_of(key), etag, size)?;
    if matches!(method, Method::Head) {
        return Response::empty().map(|r| r.with_status(200).with_headers(headers));
    }
    let object = bucket
        .get(key.to_string())
        .execute()
        .await?
        .ok_or_else(|| worker::Error::RustError("object vanished".to_string()))?;
    match object.body().and_then(|b| b.response_body().ok()) {
        Some(body) => Response::from_body(body).map(|r| r.with_status(200).with_headers(headers)),
        None => Response::empty().map(|r| r.with_status(200).with_headers(headers)),
    }
}

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if !matches!(req.method(), Method::Get | Method::Head) {
        return Response::error("method not allowed", 405);
    }
    let config = worker_config(&env)?;
    match map_asset(&config, &req.path()) {
        MappedAsset::NotFound => Response::error("not found", 404),
        MappedAsset::Direct(asset) => {
            let bucket = env.bucket(&asset.bucket_binding)?;
            let kind = match asset.kind {
                crate::map::RouteKind::ImmutableObject => RouteKind::ImmutableFile,
                crate::map::RouteKind::ViaPointer => RouteKind::GenerationPointer,
            };
            serve_object(&bucket, &asset.key, kind, &req.method(), &req.headers()).await
        }
        MappedAsset::ViaPointer { bucket_binding, pointer_key, rel_path } => {
            let bucket = env.bucket(&bucket_binding)?;
            let pointer = match bucket.get(pointer_key.clone()).execute().await? {
                Some(o) => o,
                None => return Response::error("not found", 404),
            };
            let gen_id = match pointer.body() {
                Some(body) => match body.bytes().await {
                    Ok(bytes) => match parse_pointer(&bytes) {
                        Some(g) => g,
                        None => return Response::error("not found", 404),
                    },
                    Err(_) => return Response::error("not found", 404),
                },
                None => return Response::error("not found", 404),
            };
            let key_prefix = pointer_key.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            let key = object_key_for(key_prefix, &gen_id, &rel_path);
            serve_object(&bucket, &key, RouteKind::GenerationPointer, &req.method(), &req.headers()).await
        }
    }
}
