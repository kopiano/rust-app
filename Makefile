# run postgres and redis in docker containers
docker-start:
	@docker compose up -d

docker-stop:
	@docker compose down -v

push:
	@bash push.sh

#kill -9 $(lsof -i :8100)
# run rust(axum framework) web server
run:
	@sqlx migrate run
	@cargo run

cloud:
	@cloudflared tunnel --config ~/.cloudflared/rust-app.yml --protocol http2 run rust-app
