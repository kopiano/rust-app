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

