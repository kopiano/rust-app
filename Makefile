# run postgres and redis in docker containers
docker-start:
	@docker compose up -d

docker-stop:
	@docker compose down -v

# update
docker-run:
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