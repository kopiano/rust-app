# run postgres and redis in docker containers
docker-start:
	@docker compose up -d

docker-stop:
	@docker compose down -v

# 使用现有镜像，只删除并重新创建容器。适合修改了环境变量、端口、挂载目录、Compose 配置等情况，速度较快
docker-run:
	@docker compose up -d --force-recreate backend

# 先根据 Dockerfile 重新生成镜像，再创建容器。适合修改了 Rust 代码、依赖、Dockerfile 或运行时系统依赖的情况，速度较慢
docker-build:
	@docker compose up -d --build --force-recreate backend

docker-log:
	@docker compose logs -f backend






push:
	@bash push.sh

# run rust(axum framework) web server
run:
	@sqlx migrate run
	@cargo run

load-test:
	@bash scripts/load-test.sh

kill:
	@pids=$$(lsof -t -i :8100); \
	if [ -n "$$pids" ]; then \
		kill -9 $$pids; \
	else \
		echo "No process is listening on port 8100"; \
	fi

cloud:
	@cloudflared tunnel run backend-api		# @cloudflared tunnel --config ~/.cloudflared/rust-app.yml --protocol http2 run rust-app

# 终端生成hash密码：htpasswd -bnBC 8 user '明文密码'