use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, NaiveDateTime, TimeZone, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::Json as SqlJson;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::app::AppState;

const YAHOO_CHART_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const NASDAQ_API_URL: &str = "https://api.nasdaq.com/api/quote";
const TENCENT_KLINE_URL: &str = "https://ifzq.gtimg.cn/appstock/app/kline/mkline";
const TENCENT_DAILY_URL: &str = "https://ifzq.gtimg.cn/appstock/app/fqkline/get";
const STOCK_CACHE_TTL: Duration = Duration::from_secs(45);
const STOCK_STALE_TTL: Duration = Duration::from_secs(5 * 60);
const STOCK_BATCH_CONCURRENCY: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct StockCatalogItem {
    pub symbol: &'static str,
    pub name: &'static str,
    pub market: &'static str,
    pub category: &'static str,
}

const CATALOG: &[StockCatalogItem] = &[
    StockCatalogItem {
        symbol: "600519.SS",
        name: "贵州茅台",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "601398.SS",
        name: "工商银行",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "601318.SS",
        name: "中国平安",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "600036.SS",
        name: "招商银行",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "000858.SZ",
        name: "五粮液",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "000651.SZ",
        name: "格力电器",
        market: "CN",
        category: "国内大公司",
    },
    StockCatalogItem {
        symbol: "688825.SS",
        name: "长鑫科技",
        market: "CN",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "688836.SS",
        name: "宇树科技",
        market: "CN",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "002624.SZ",
        name: "完美世界",
        market: "CN",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "688180.SS",
        name: "君实生物",
        market: "CN",
        category: "生物医药",
    },
    StockCatalogItem {
        symbol: "688981.SS",
        name: "中芯国际",
        market: "CN",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "300750.SZ",
        name: "宁德时代",
        market: "CN",
        category: "新能源",
    },
    StockCatalogItem {
        symbol: "002594.SZ",
        name: "比亚迪",
        market: "CN",
        category: "新能源",
    },
    StockCatalogItem {
        symbol: "0700.HK",
        name: "腾讯控股",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "9988.HK",
        name: "阿里巴巴",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "3690.HK",
        name: "美团",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "1810.HK",
        name: "小米集团",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "9626.HK",
        name: "哔哩哔哩",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "9618.HK",
        name: "京东集团",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "9999.HK",
        name: "网易",
        market: "HK",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "BIDU",
        name: "百度",
        market: "US",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "PDD",
        name: "拼多多",
        market: "US",
        category: "国内科技",
    },
    StockCatalogItem {
        symbol: "AAPL",
        name: "Apple",
        market: "US",
        category: "海外科技",
    },
    StockCatalogItem {
        symbol: "MSFT",
        name: "Microsoft",
        market: "US",
        category: "海外科技",
    },
    StockCatalogItem {
        symbol: "NVDA",
        name: "NVIDIA",
        market: "US",
        category: "海外科技",
    },
    StockCatalogItem {
        symbol: "GOOGL",
        name: "Alphabet",
        market: "US",
        category: "海外大公司",
    },
    StockCatalogItem {
        symbol: "AMZN",
        name: "Amazon",
        market: "US",
        category: "海外大公司",
    },
    StockCatalogItem {
        symbol: "META",
        name: "Meta Platforms",
        market: "US",
        category: "海外科技",
    },
    StockCatalogItem {
        symbol: "TSLA",
        name: "Tesla",
        market: "US",
        category: "海外科技",
    },
    StockCatalogItem {
        symbol: "JPM",
        name: "JPMorgan Chase",
        market: "US",
        category: "海外大公司",
    },
];

#[derive(Debug, Deserialize)]
pub struct StockQuery {
    pub symbols: Option<String>,
    pub range: Option<String>,
    pub interval: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockResponse {
    pub symbol: String,
    pub name: String,
    pub market: String,
    pub category: String,
    pub currency: Option<String>,
    pub exchange: Option<String>,
    pub price: Option<f64>,
    pub previous_close: Option<f64>,
    pub change: Option<f64>,
    pub percent: Option<f64>,
    pub market_cap: Option<f64>,
    pub as_of: Option<String>,
    pub points: Vec<StockPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockPoint {
    pub timestamp: i64,
    pub time: String,
    pub close: f64,
}

#[derive(Debug, Clone)]
struct CachedStock {
    response: StockResponse,
    fetched_at: Instant,
}

#[derive(Debug, sqlx::FromRow)]
struct StockCacheRow {
    symbol: String,
    range: String,
    interval: String,
    response: SqlJson<StockResponse>,
    fetched_at: DateTime<Utc>,
}

struct StockFlight {
    result: Mutex<Option<Result<StockResponse, (StatusCode, String)>>>,
    notify: Notify,
}

static STOCK_CACHE: OnceLock<RwLock<HashMap<String, CachedStock>>> = OnceLock::new();
static STOCK_REFRESHING: OnceLock<RwLock<HashSet<String>>> = OnceLock::new();
static STOCK_IN_FLIGHT: OnceLock<Mutex<HashMap<String, Arc<StockFlight>>>> = OnceLock::new();

fn stock_cache() -> &'static RwLock<HashMap<String, CachedStock>> {
    STOCK_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn stock_refreshing() -> &'static RwLock<HashSet<String>> {
    STOCK_REFRESHING.get_or_init(|| RwLock::new(HashSet::new()))
}

fn stock_in_flight() -> &'static Mutex<HashMap<String, Arc<StockFlight>>> {
    STOCK_IN_FLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn catalog() -> Json<Vec<StockCatalogItem>> {
    Json(CATALOG.to_vec())
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<StockQuery>,
) -> Result<Json<Vec<StockResponse>>, (StatusCode, String)> {
    let validated_range = normalize_range(query.range.as_deref())?;
    let (validated_interval, source_interval, bucket_minutes) =
        normalize_interval(validated_range, query.interval.as_deref())?;
    let requested_symbols = query.symbols.clone().unwrap_or_else(|| {
        CATALOG
            .iter()
            .map(|item| item.symbol)
            .collect::<Vec<_>>()
            .join(",")
    });
    let symbols = requested_symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty() && valid_symbol(symbol))
        .take(30)
        .map(str::to_uppercase)
        .collect::<Vec<_>>();
    if symbols.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "no valid stock symbols".to_string(),
        ));
    }

    preload_db_cache(&state.db, &symbols, validated_range, validated_interval).await;

    let responses = futures_util::stream::iter(symbols.into_iter().enumerate().map(
        |(index, symbol)| {
            let state = state.clone();
            async move {
                (
                    index,
                    fetch_stock_normalized(
                        &state,
                        &symbol,
                        validated_range,
                        validated_interval,
                        source_interval,
                        bucket_minutes,
                    )
                    .await,
                )
            }
        },
    ))
    .buffer_unordered(STOCK_BATCH_CONCURRENCY)
    .filter_map(|result| async move {
        match result {
            (index, Ok(response)) => Some((index, response)),
            (index, Err((status, message))) => {
                tracing::warn!(%status, %message, "skipping unavailable stock in batch response");
                let _ = index;
                None
            }
        }
    })
    .collect::<Vec<_>>()
    .await;
    let mut responses = responses;
    responses.sort_unstable_by_key(|(index, _)| *index);
    let responses = responses
        .into_iter()
        .map(|(_, response)| response)
        .collect::<Vec<_>>();
    if responses.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "stock data provider unavailable".to_string(),
        ));
    }
    Ok(Json(responses))
}

pub async fn get(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    Query(query): Query<StockQuery>,
) -> Result<Json<StockResponse>, (StatusCode, String)> {
    let symbol = symbol.trim().to_uppercase();
    if !valid_symbol(&symbol) {
        return Err((StatusCode::BAD_REQUEST, "invalid stock symbol".to_string()));
    }
    Ok(Json(
        fetch_stock(
            &state,
            &symbol,
            query.range.as_deref(),
            query.interval.as_deref(),
        )
        .await?,
    ))
}

async fn fetch_stock(
    state: &AppState,
    symbol: &str,
    range: Option<&str>,
    interval: Option<&str>,
) -> Result<StockResponse, (StatusCode, String)> {
    let range = normalize_range(range)?;
    let (interval, source_interval, bucket_minutes) = normalize_interval(range, interval)?;
    fetch_stock_normalized(
        state,
        symbol,
        range,
        interval,
        source_interval,
        bucket_minutes,
    )
    .await
}

async fn fetch_stock_normalized(
    state: &AppState,
    symbol: &str,
    range: &str,
    interval: &str,
    source_interval: &str,
    bucket_minutes: Option<u64>,
) -> Result<StockResponse, (StatusCode, String)> {
    let cache_key = format!("{symbol}:{range}:{interval}");
    let stale = {
        let cache = stock_cache().read().await;
        cache.get(&cache_key).cloned()
    };
    let stale = match stale {
        Some(entry) => Some(entry),
        None => load_db_cache(&state.db, symbol, range, interval, &cache_key).await,
    };

    if let Some(entry) = stale.as_ref()
        && entry.fetched_at.elapsed() < STOCK_CACHE_TTL
    {
        return Ok(entry.response.clone());
    }

    if let Some(entry) = stale.as_ref()
        && entry.fetched_at.elapsed() < STOCK_STALE_TTL
    {
        spawn_refresh(
            state.clone(),
            symbol.to_string(),
            range.to_string(),
            interval.to_string(),
            source_interval,
            bucket_minutes,
            cache_key,
        )
        .await;
        return Ok(entry.response.clone());
    }

    let (flight, leader) = start_or_join_stock_flight(&cache_key).await;
    if !leader {
        return wait_for_stock_flight(flight).await;
    }

    let result = fetch_remote_stock(
        state,
        symbol,
        range,
        interval,
        source_interval,
        bucket_minutes,
    )
    .await;

    let result = match result {
        Ok(response) => {
            save_stock_cache(state, &cache_key, symbol, range, interval, &response).await;
            Ok(response)
        }
        Err(error) => {
            if let Some(entry) = stale {
                tracing::warn!(symbol, %range, "stock provider failed; serving stale real stock data");
                Ok(entry.response)
            } else {
                Err(error)
            }
        }
    };
    finish_stock_flight(&cache_key, flight, result.clone()).await;
    result
}

async fn fetch_remote_stock(
    state: &AppState,
    symbol: &str,
    range: &str,
    interval: &str,
    source_interval: &str,
    bucket_minutes: Option<u64>,
) -> Result<StockResponse, (StatusCode, String)> {
    if is_cn_hk_symbol(symbol) {
        let primary = fetch_tencent_stock(state, symbol, range, interval, bucket_minutes).await;
        match primary {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(symbol, %range, %interval, "Tencent stock provider failed; trying Yahoo");
                fetch_yahoo_stock(state, symbol, range, source_interval, bucket_minutes)
                    .await
                    .map_err(|_| error)
            }
        }
    } else if is_us_symbol(symbol) {
        let primary = if range == "5d" {
            fetch_yahoo_stock(state, symbol, range, source_interval, bucket_minutes).await
        } else {
            fetch_nasdaq_stock(state, symbol, range).await
        };
        match primary {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(symbol, %range, %interval, "primary stock provider failed; trying fallback");
                let fallback = if range == "5d" {
                    fetch_nasdaq_stock(state, symbol, range).await
                } else {
                    fetch_yahoo_stock(state, symbol, range, source_interval, bucket_minutes).await
                };
                fallback.map_err(|_| error)
            }
        }
    } else {
        fetch_yahoo_stock(state, symbol, range, source_interval, bucket_minutes).await
    }
}

async fn preload_db_cache(db: &sqlx::PgPool, symbols: &[String], range: &str, interval: &str) {
    let symbols = {
        let cache = stock_cache().read().await;
        symbols
            .iter()
            .filter(|symbol| {
                cache
                    .get(&format!("{symbol}:{range}:{interval}"))
                    .map(|entry| entry.fetched_at.elapsed() >= STOCK_STALE_TTL)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if symbols.is_empty() {
        return;
    }

    let rows = sqlx::query_as::<_, StockCacheRow>(
        r#"
        SELECT symbol, range, interval, response, fetched_at
        FROM stock_quote_cache
        WHERE symbol = ANY($1)
          AND range = $2
          AND interval = $3
        "#,
    )
    .bind(&symbols)
    .bind(range)
    .bind(interval)
    .fetch_all(db)
    .await;

    let Ok(rows) = rows else {
        return;
    };
    let now = Utc::now();
    let mut cache = stock_cache().write().await;
    for row in rows {
        let age = Duration::from_secs((now - row.fetched_at).num_seconds().max(0) as u64);
        if age < STOCK_STALE_TTL {
            let cache_key = format!("{}:{}:{}", row.symbol, row.range, row.interval);
            let should_insert = cache
                .get(&cache_key)
                .map(|entry| entry.fetched_at.elapsed() > age)
                .unwrap_or(true);
            if should_insert {
                cache.insert(
                    cache_key,
                    CachedStock {
                        response: row.response.0,
                        fetched_at: Instant::now() - age,
                    },
                );
            }
        }
    }
}

async fn start_or_join_stock_flight(cache_key: &str) -> (Arc<StockFlight>, bool) {
    let mut flights = stock_in_flight().lock().await;
    if let Some(flight) = flights.get(cache_key) {
        return (Arc::clone(flight), false);
    }
    let flight = Arc::new(StockFlight {
        result: Mutex::new(None),
        notify: Notify::new(),
    });
    flights.insert(cache_key.to_string(), Arc::clone(&flight));
    (flight, true)
}

async fn wait_for_stock_flight(
    flight: Arc<StockFlight>,
) -> Result<StockResponse, (StatusCode, String)> {
    loop {
        if let Some(result) = flight.result.lock().await.clone() {
            return result;
        }
        flight.notify.notified().await;
    }
}

async fn finish_stock_flight(
    cache_key: &str,
    flight: Arc<StockFlight>,
    result: Result<StockResponse, (StatusCode, String)>,
) {
    *flight.result.lock().await = Some(result);
    flight.notify.notify_waiters();
    stock_in_flight().lock().await.remove(cache_key);
}

async fn load_db_cache(
    db: &sqlx::PgPool,
    symbol: &str,
    range: &str,
    interval: &str,
    cache_key: &str,
) -> Option<CachedStock> {
    let row = sqlx::query_as::<_, StockCacheRow>(
        r#"
        SELECT symbol, range, interval, response, fetched_at
        FROM stock_quote_cache
        WHERE symbol = $1
          AND range = $2
          AND interval = $3
        "#,
    )
    .bind(symbol)
    .bind(range)
    .bind(interval)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    let age = Duration::from_secs((Utc::now() - row.fetched_at).num_seconds().max(0) as u64);
    if age >= STOCK_STALE_TTL {
        return None;
    }
    let entry = CachedStock {
        response: row.response.0,
        fetched_at: Instant::now() - age,
    };
    stock_cache()
        .write()
        .await
        .insert(cache_key.to_string(), entry.clone());
    Some(entry)
}

async fn spawn_refresh(
    state: AppState,
    symbol: String,
    range: String,
    interval: String,
    source_interval: &str,
    bucket_minutes: Option<u64>,
    cache_key: String,
) {
    {
        let mut refreshing = stock_refreshing().write().await;
        if !refreshing.insert(cache_key.clone()) {
            return;
        }
    }
    let source_interval = source_interval.to_string();
    tokio::spawn(async move {
        let result = fetch_remote_stock(
            &state,
            &symbol,
            &range,
            &interval,
            &source_interval,
            bucket_minutes,
        )
        .await;
        if let Ok(response) = result {
            save_stock_cache(&state, &cache_key, &symbol, &range, &interval, &response).await;
        }
        stock_refreshing().write().await.remove(&cache_key);
    });
}

async fn save_stock_cache(
    state: &AppState,
    cache_key: &str,
    symbol: &str,
    range: &str,
    interval: &str,
    response: &StockResponse,
) {
    stock_cache().write().await.insert(
        cache_key.to_string(),
        CachedStock {
            response: response.clone(),
            fetched_at: Instant::now(),
        },
    );
    let db = state.db.clone();
    let symbol = symbol.to_string();
    let range = range.to_string();
    let interval = interval.to_string();
    let response = response.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            r#"
            INSERT INTO stock_quote_cache (symbol, range, interval, response, fetched_at)
            VALUES ($1, $2, $3, $4, NOW())
            ON CONFLICT (symbol, range, interval)
            DO UPDATE SET response = EXCLUDED.response, fetched_at = EXCLUDED.fetched_at
            "#,
        )
        .bind(symbol)
        .bind(range)
        .bind(interval)
        .bind(SqlJson(response))
        .execute(&db)
        .await;
    });
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::warn!(%error, "stock provider request failed");
    (
        StatusCode::BAD_GATEWAY,
        "stock data provider unavailable".to_string(),
    )
}

fn normalize_range(range: Option<&str>) -> Result<&str, (StatusCode, String)> {
    let range = range.unwrap_or("1mo");
    match range {
        "1d" | "5d" | "1mo" | "3mo" | "6mo" | "1y" | "5y" | "max" => Ok(range),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "unsupported stock range".to_string(),
        )),
    }
}

fn normalize_interval(
    range: &str,
    interval: Option<&str>,
) -> Result<(&'static str, &'static str, Option<u64>), (StatusCode, String)> {
    let requested = interval.unwrap_or(match range {
        "1d" => "1m",
        "5d" => "10m",
        "1mo" | "3mo" => "1h",
        _ => "1d",
    });
    match requested {
        "1m" => Ok(("1m", "1m", None)),
        "5m" => Ok(("5m", "5m", None)),
        "10m" => Ok(("10m", "5m", Some(10))),
        "15m" => Ok(("15m", "15m", None)),
        "30m" => Ok(("30m", "30m", None)),
        "60m" | "1h" => Ok(("1h", "1h", None)),
        "1d" => Ok(("1d", "1d", None)),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "unsupported stock interval".to_string(),
        )),
    }
}

fn is_us_symbol(symbol: &str) -> bool {
    CATALOG
        .iter()
        .find(|item| item.symbol == symbol)
        .map(|item| item.market == "US")
        .unwrap_or(!symbol.contains('.'))
}

fn is_cn_hk_symbol(symbol: &str) -> bool {
    CATALOG
        .iter()
        .find(|item| item.symbol == symbol)
        .map(|item| item.market == "CN" || item.market == "HK")
        .unwrap_or_else(|| {
            symbol.ends_with(".SS") || symbol.ends_with(".SZ") || symbol.ends_with(".HK")
        })
}

fn china_hong_kong_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 60 * 60).expect("UTC+08:00 is a valid fixed offset")
}

fn local_timestamp(value: &str, format: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(value, format).ok()?;
    china_hong_kong_offset()
        .from_local_datetime(&naive)
        .single()
        .map(|date| date.with_timezone(&Utc).timestamp())
}

fn local_market_date(timestamp: i64) -> Option<chrono::NaiveDate> {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|date| date.with_timezone(&china_hong_kong_offset()).date_naive())
}

async fn fetch_tencent_stock(
    state: &AppState,
    symbol: &str,
    range: &str,
    interval: &str,
    bucket_minutes: Option<u64>,
) -> Result<StockResponse, (StatusCode, String)> {
    let catalog = CATALOG.iter().find(|item| item.symbol == symbol);
    let market = catalog
        .map(|item| item.market)
        .unwrap_or_else(|| if symbol.ends_with(".HK") { "HK" } else { "CN" });
    let quote_symbol = match market {
        "HK" => format!("hk{:0>5}", symbol.trim_end_matches(".HK")),
        _ if symbol.ends_with(".SZ") => format!("sz{}", symbol.trim_end_matches(".SZ")),
        _ => format!("sh{}", symbol.trim_end_matches(".SS")),
    }
    .to_lowercase();
    let (points, price, previous_close) =
        if is_cn_hk_symbol(symbol) && (range == "1d" || range == "5d") {
            let source_interval = if range == "1d" && interval == "1m" {
                "m1"
            } else {
                "m5"
            };
            let url = format!("{TENCENT_KLINE_URL}?param={quote_symbol},{source_interval},,640");
            let payload = fetch_json(state, url, "Tencent intraday").await?;
            let data = &payload["data"][&quote_symbol];
            let rows = data[source_interval].as_array().ok_or_else(|| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("no Tencent intraday data returned for {symbol}"),
                )
            })?;
            let mut points = rows
                .iter()
                .filter_map(|row| {
                    let values = row.as_array()?;
                    let timestamp = local_timestamp(values.first()?.as_str()?, "%Y%m%d%H%M")?;
                    let close = values.get(2).and_then(parse_number)?;
                    Some(StockPoint {
                        timestamp,
                        time: DateTime::<Utc>::from_timestamp(timestamp, 0)?.to_rfc3339(),
                        close,
                    })
                })
                .collect::<Vec<_>>();
            if points.is_empty() {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("no Tencent intraday points returned for {symbol}"),
                ));
            }
            points.sort_unstable_by_key(|point| point.timestamp);
            let latest_date = points
                .last()
                .and_then(|point| local_market_date(point.timestamp));
            if range == "1d" {
                points.retain(|point| local_market_date(point.timestamp) == latest_date);
            } else {
                let cutoff_date = latest_date.map(|date| date - ChronoDuration::days(4));
                points.retain(|point| {
                    local_market_date(point.timestamp)
                        .zip(cutoff_date)
                        .is_some_and(|(date, cutoff)| date >= cutoff)
                });
            }
            let points = match bucket_minutes {
                Some(minutes) => bucket_points(points, minutes),
                None => points,
            };
            let quote = data["qt"][&quote_symbol].as_array();
            let price = quote
                .and_then(|values| values.get(3))
                .and_then(parse_number)
                .or_else(|| points.last().map(|point| point.close));
            let previous_close = quote
                .and_then(|values| values.get(4))
                .and_then(parse_number);
            (points, price, previous_close)
        } else {
            let url = format!("{TENCENT_DAILY_URL}?param={quote_symbol},day,,,320,1");
            let payload = fetch_json(state, url, "Tencent daily").await?;
            let rows = payload["data"][&quote_symbol]["day"]
                .as_array()
                .ok_or_else(|| {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("no Tencent daily data returned for {symbol}"),
                    )
                })?;
            let mut all_points = rows
                .iter()
                .filter_map(|row| {
                    let values = row.as_array()?;
                    let timestamp = local_timestamp(
                        &format!("{} 00:00", values.first()?.as_str()?),
                        "%Y-%m-%d %H:%M",
                    )?;
                    let close = values.get(2).and_then(parse_number)?;
                    Some(StockPoint {
                        timestamp,
                        time: DateTime::<Utc>::from_timestamp(timestamp, 0)?.to_rfc3339(),
                        close,
                    })
                })
                .collect::<Vec<_>>();
            all_points.sort_unstable_by_key(|point| point.timestamp);
            let price = all_points.last().map(|point| point.close);
            let previous_close = all_points
                .get(all_points.len().saturating_sub(2))
                .map(|point| point.close);
            if range == "1d" {
                let latest_date = all_points
                    .last()
                    .and_then(|point| local_market_date(point.timestamp));
                all_points.retain(|point| local_market_date(point.timestamp) == latest_date);
            } else if range == "5d" {
                let latest_date = all_points
                    .last()
                    .and_then(|point| local_market_date(point.timestamp));
                let cutoff_date = latest_date.map(|date| date - ChronoDuration::days(4));
                all_points.retain(|point| {
                    local_market_date(point.timestamp)
                        .zip(cutoff_date)
                        .is_some_and(|(date, cutoff)| date >= cutoff)
                });
            }
            let points = all_points;
            (points, price, previous_close)
        };
    let change = price
        .zip(previous_close)
        .map(|(price, previous)| price - previous);
    let percent = change
        .zip(previous_close)
        .and_then(|(change, previous)| (previous != 0.0).then_some(change / previous * 100.0));
    Ok(StockResponse {
        symbol: symbol.to_string(),
        name: catalog.map(|item| item.name).unwrap_or(symbol).to_string(),
        market: market.to_string(),
        category: catalog
            .map(|item| item.category)
            .unwrap_or("其他")
            .to_string(),
        currency: Some(if market == "HK" { "HKD" } else { "CNY" }.to_string()),
        exchange: Some(if market == "HK" { "HKEX" } else { "SSE/SZSE" }.to_string()),
        price,
        previous_close,
        change,
        percent,
        market_cap: None,
        as_of: Some(Utc::now().to_rfc3339()),
        points,
    })
}

async fn fetch_json(
    state: &AppState,
    url: String,
    provider: &str,
) -> Result<Value, (StatusCode, String)> {
    let response = state
        .stock_client
        .get(url)
        .header("Accept", "application/json")
        .header("Referer", "https://stockapp.finance.qq.com/")
        .header("User-Agent", "rust-app-stock-service/1.0")
        .send()
        .await
        .map_err(internal_error)?;
    if !response.status().is_success() {
        return Err(internal_error(format!(
            "{provider} returned {}",
            response.status()
        )));
    }
    response.json().await.map_err(internal_error)
}

async fn fetch_yahoo_stock(
    state: &AppState,
    symbol: &str,
    range: &str,
    source_interval: &str,
    bucket_minutes: Option<u64>,
) -> Result<StockResponse, (StatusCode, String)> {
    let url = format!(
        "{YAHOO_CHART_URL}/{symbol}?range={range}&interval={source_interval}&events=div%2Csplits"
    );
    let response = state
        .stock_client
        .get(url)
        .header("Accept", "application/json")
        .header("Referer", "https://finance.yahoo.com/")
        .header("User-Agent", "rust-app-stock-service/1.0")
        .send()
        .await
        .map_err(internal_error)?;
    if !response.status().is_success() {
        return Err(internal_error(format!(
            "Yahoo returned {}",
            response.status()
        )));
    }
    let payload: Value = response.json().await.map_err(internal_error)?;
    let result = payload["chart"]["result"].get(0).ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            format!("no stock data returned for {symbol}"),
        )
    })?;
    let meta = &result["meta"];
    let quote = result["indicators"]["quote"].get(0).unwrap_or(&Value::Null);
    let timestamps = result["timestamp"].as_array().cloned().unwrap_or_default();
    let closes = quote["close"].as_array().cloned().unwrap_or_default();
    let points = timestamps
        .iter()
        .zip(closes.iter())
        .filter_map(|(timestamp, close)| {
            let timestamp = timestamp.as_i64()?;
            let close = close.as_f64()?;
            let time = DateTime::<Utc>::from_timestamp(timestamp, 0)?.to_rfc3339();
            Some(StockPoint {
                timestamp,
                time,
                close,
            })
        })
        .collect::<Vec<_>>();
    let points = match bucket_minutes {
        Some(minutes) => bucket_points(points, minutes),
        None => points,
    };
    let price = meta["regularMarketPrice"]
        .as_f64()
        .or_else(|| points.last().map(|point| point.close));
    let previous_close = meta["previousClose"].as_f64();
    let change = price
        .zip(previous_close)
        .map(|(price, previous)| price - previous);
    let percent = change
        .zip(previous_close)
        .and_then(|(change, previous)| (previous != 0.0).then_some(change / previous * 100.0));
    let catalog = CATALOG.iter().find(|item| item.symbol == symbol);

    Ok(StockResponse {
        symbol: symbol.to_string(),
        name: catalog.map(|item| item.name).unwrap_or(symbol).to_string(),
        market: catalog
            .map(|item| item.market)
            .unwrap_or("UNKNOWN")
            .to_string(),
        category: catalog
            .map(|item| item.category)
            .unwrap_or("其他")
            .to_string(),
        currency: meta["currency"].as_str().map(str::to_string),
        exchange: meta["exchangeName"].as_str().map(str::to_string),
        price,
        previous_close,
        change,
        percent,
        market_cap: meta["marketCap"].as_f64(),
        as_of: meta["regularMarketTime"].as_i64().and_then(|timestamp| {
            DateTime::<Utc>::from_timestamp(timestamp, 0).map(|date| date.to_rfc3339())
        }),
        points,
    })
}

async fn fetch_nasdaq_stock(
    state: &AppState,
    symbol: &str,
    range: &str,
) -> Result<StockResponse, (StatusCode, String)> {
    let url = if range == "1d" {
        format!("{NASDAQ_API_URL}/{symbol}/chart?assetclass=stocks")
    } else {
        let today = Utc::now().date_naive();
        let days = match range {
            "5d" => 7,
            "1mo" => 35,
            "3mo" => 100,
            "6mo" => 190,
            "1y" => 380,
            "5y" => 1900,
            _ => 10_000,
        };
        let from = today - ChronoDuration::days(days);
        format!("{NASDAQ_API_URL}/{symbol}/chart?assetclass=stocks&fromdate={from}&todate={today}")
    };
    let response = state
        .stock_client
        .get(url)
        .header("Accept", "application/json")
        .header("Origin", "https://www.nasdaq.com")
        .header("Referer", "https://www.nasdaq.com/")
        .header("User-Agent", "rust-app-stock-service/1.0")
        .send()
        .await
        .map_err(internal_error)?;
    if !response.status().is_success() {
        return Err(internal_error(format!(
            "Nasdaq returned {}",
            response.status()
        )));
    }
    let payload: Value = response.json().await.map_err(internal_error)?;
    let data = payload["data"].as_object().ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            format!("no Nasdaq data returned for {symbol}"),
        )
    })?;
    let primary = data
        .get("lastSalePrice")
        .and_then(parse_number)
        .or_else(|| data.get("chart")?.as_array()?.last()?.get("y")?.as_f64());
    let previous_close = data.get("previousClose").and_then(parse_number);
    let change = data.get("netChange").and_then(parse_number).or_else(|| {
        primary
            .zip(previous_close)
            .map(|(price, previous)| price - previous)
    });
    let percent = data
        .get("percentageChange")
        .and_then(parse_number)
        .or_else(|| {
            change.zip(previous_close).and_then(|(change, previous)| {
                (previous != 0.0).then_some(change / previous * 100.0)
            })
        });
    let points = data
        .get("chart")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|point| {
            let timestamp = point["x"].as_i64()? / 1000;
            let close = point["y"].as_f64()?;
            Some(StockPoint {
                timestamp,
                time: DateTime::<Utc>::from_timestamp(timestamp, 0)?.to_rfc3339(),
                close,
            })
        })
        .collect::<Vec<_>>();
    let catalog = CATALOG.iter().find(|item| item.symbol == symbol);
    Ok(StockResponse {
        symbol: symbol.to_string(),
        name: catalog.map(|item| item.name).unwrap_or(symbol).to_string(),
        market: catalog.map(|item| item.market).unwrap_or("US").to_string(),
        category: catalog
            .map(|item| item.category)
            .unwrap_or("海外股票")
            .to_string(),
        currency: Some("USD".to_string()),
        exchange: data
            .get("exchange")
            .and_then(Value::as_str)
            .map(str::to_string),
        price: primary,
        previous_close,
        change,
        percent,
        market_cap: None,
        as_of: Some(Utc::now().to_rfc3339()),
        points,
    })
}

fn parse_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value
            .trim()
            .trim_start_matches('$')
            .trim_end_matches('%')
            .replace(',', "")
            .parse()
            .ok(),
        _ => None,
    }
}

fn valid_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= 20
        && symbol.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
}

fn bucket_points(points: Vec<StockPoint>, minutes: u64) -> Vec<StockPoint> {
    if points.len() < 2 {
        return points;
    }
    let bucket_seconds = (minutes * 60) as i64;
    let mut buckets: Vec<StockPoint> = Vec::with_capacity(points.len());
    for point in points {
        let bucket = point.timestamp / bucket_seconds * bucket_seconds;
        if let Some(previous) = buckets.last_mut()
            && previous.timestamp == bucket
        {
            *previous = StockPoint {
                timestamp: bucket,
                time: DateTime::<Utc>::from_timestamp(bucket, 0)
                    .map(|date| date.to_rfc3339())
                    .unwrap_or(point.time),
                close: point.close,
            };
        } else {
            buckets.push(StockPoint {
                timestamp: bucket,
                time: DateTime::<Utc>::from_timestamp(bucket, 0)
                    .map(|date| date.to_rfc3339())
                    .unwrap_or(point.time.clone()),
                close: point.close,
            });
        }
    }
    buckets
}

impl IntoResponse for StockResponse {
    fn into_response(self) -> axum::response::Response {
        Json(self).into_response()
    }
}
