# run postgres and redis in docker containers
docker-start:
	@docker compose up -d

docker-stop:
	@docker compose down -v

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
	@cloudflared tunnel --config ~/.cloudflared/rust-app.yml --protocol http2 run rust-app
