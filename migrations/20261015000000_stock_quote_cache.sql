CREATE TABLE IF NOT EXISTS stock_quote_cache (
    symbol TEXT NOT NULL,
    range TEXT NOT NULL,
    interval TEXT NOT NULL,
    response JSONB NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (symbol, range, interval)
);

CREATE INDEX IF NOT EXISTS stock_quote_cache_fetched_at_idx
    ON stock_quote_cache (fetched_at DESC);

CREATE INDEX IF NOT EXISTS stock_quote_cache_range_interval_symbol_idx
    ON stock_quote_cache (range, interval, symbol);
