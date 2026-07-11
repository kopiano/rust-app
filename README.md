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


### router
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