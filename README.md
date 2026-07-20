# rust-app

## Overview
### technology stack
* language: rust
* Web Framework: Axnm
* Database: PostgreSQL
* Cache：Redis
* ORM：sqlx
* Docker + Docker Compose
* Runtime：Tokio
* Middleware：CORS
* Authentication：JWT
* Configuration：dotenvy
* Logging：tracing
* cookie保存jwt
* 实时通信：websocket
* 视频转码：ffmpeg

### install
* 未创建项目目录
```sh
$ cargo new rust-app
$ cd rust-app
$ cargo run
```

* 在该项目目录下
```sh
$ cargo init
$ cargo run
```

### Dependency Library
```sh
cargo add axum tokio --features tokio/full    # axum
cargo add sqlx --features runtime-tokio-rustls,postgres,uuid,chrono   # postgresql(sqlx)
cargo add redis        # redis
cargo add dotenvy      # .env
cargo add tower-http --features cors # cors
cargo add jsonwebtoken # jwt
cargo add bcrypt       # password encryption
```
-F 是 --features 的缩写，用于启用 crate 的特定特性

### run
本机启动axum项目，docker部署postgresql和redis

### postgresql
```sh
brew install postgresql@18      # macos need update xcode@latest
brew services start postgresql@18
```

### sqlx(orm)
```shell
cargo install sqlx-cli                                                                                                                                                   
sqlx migrate run
```


### tree 参考
```
src/
├── database/
│   ├── mod.rs
│   └── postgres.rs      # 数据库连接
│
├── models/
│   └── user.rs          # 数据模型
│
├── modules/
│   └── user/
│       ├── handler.rs   # HTTP 请求处理
│       ├── service.rs   # 业务逻辑
│       ├── repository.rs# SQLx 查询
│       ├── dto.rs       # 请求/响应 DTO
│       ├── routes.rs    # 路由
│       └── mod.rs
│
├── state.rs             # AppState
├── app.rs               # Router 配置
└── main.rs              # 程序入口
```

```shell
.
|
├── src
│       ├── app
│       │       ├── mod.rs
│       │       ├── router.rs
│       │       └── state.rs
│       ├── config
│       │       ├── jwt.rs
│       │       ├── logger.rs
│       │       └── mod.rs
│       ├── database
│       │       ├── mod.rs
│       │       ├── postgres.rs
│       │       └── redis.rs
│       ├── handles
│       │       ├── auth.rs
│       │       ├── mod.rs
│       │       ├── task.rs
│       │       └── user.rs
│       ├── main.rs
│       ├── middleware
│       │       ├── cors.rs
│       │       ├── jwt.rs
│       │       ├── logger.rs
│       │       └── mod.rs
│       └── models
│             ├── mod.rs
│             ├── task.rs
│             └── user.rs
|
├── migrations
│       ├── 20260711113801_create_user_table.sql
│       ├── 20260711113900_add_password_to_user.sql
│       └── 20260711114000_create_task_table.sql
├── push.sh
├── API.md
├── Cargo.lock
├── Cargo.toml
├── Claude.md
├── LICENSE
├── Makefile
├── README.md
├── docker-compose.yml
└── target
```

### git add .撤回
```sh
git restore --staged .
```



## 接口性能优化
### 并发与背压

服务默认使用以下并发限制，可通过 `.env` 调整：

```text
HTTP_CONCURRENCY=128
UPLOAD_CONCURRENCY=4
BCRYPT_CONCURRENCY=4
TRANSCODE_CONCURRENCY=2
WS_CONNECTION_QUEUE_CAPACITY=64
DB_MAX_CONNECTIONS=20
DB_ACQUIRE_TIMEOUT_MS=2000
```

运行状态：

```shell
curl http://127.0.0.1:8100/api/health
curl http://127.0.0.1:8100/api/metrics
```

安装 `hey` 后运行基础压测：

```shell
brew install hey
make load-test

CONCURRENCY=128 REQUESTS=10000 TARGET_PATH=/api/health make load-test
```

压测时关注 P95/P99 延迟、`http.rejected`、数据库连接池占用和 WebSocket
慢连接丢弃数量。逐级测试 `32`、`64`、`128` 并发，不要直接将生产限制调到压测峰值。

### login
* bcrypt校验耗时严重，降低cost。
```rust
const BCRYPT_COST: u32 = 8;
```
* 使用 Tokio 的阻塞线程池
```rust
let is_valid = tokio::task::spawn_blocking(move || {
    bcrypt::verify(password, &hash)
})
.await??;
```


## 日志
库：tracing
输出格式：
* 终端控制台：一行彩色
* 日志文件：json，便于按字段筛选


终端控制台输出格式：
  * 彩色、单行文本日志（便于终端阅读）
  * 固定列宽，左对齐，只有耗时和状态码右对齐
  * path缩写，user_id缩写，sql语句缩写，其它太长也缩写，不要把其他字段挤出其位置
  * 日志级别：INFO绿色，warn橙色，Error红色。
  * 状态：OK或200绿色，4开头红色，5开头橙色
  * 耗时：>100 ms橙色，>1000 ms红色，耗时预留5位数字的位置
  * 日志的path如果太长要省略一些不重要的内容用*表示，比如user_id只显示9b9fd548-***这样子
```sh
2026-07-17 21:45:18  INFO     app::server   > Server started
2026-07-17 21:45:18  INFO     app::db       > PostgreSQL connected
2026-07-17 21:45:18  INFO     app::redis    > Redis connected
2026-07-17 21:45:18  INFO     app::http     > Listening on 0.0.0.0:8100
2026-07-17 21:45:18  INFO     app::sql      > INSERT music                  OK        8 ms  rows=1
2026-07-17 21:45:18  INFO     app::ws       > CONNECT                       OK        1 ms  user=1001
2026-07-17 21:45:18  INFO     app::ffmpeg   > CONNECT                       OK     3284 ms  music=105        AAC 128kbps
2026-07-16 18:45:37  INFO     GET           /api/music                     200       0 ms
2026-07-16 18:31:16  INFO     POST          /api/music/upload              201      91 ms   user_id=1001     Upload success (id=105)
2026-07-16 18:32:12  Warn     POST          /api/login                     401      18 ms   user_id=1001
2026-07-16 18:34:29  Error    GET           /api/music                     500       2 ms                    Database timeout
```
