# run postgres and redis in docker containers
docker-start:
	@docker compose up -d

docker-stop:
	@docker compose down -v

# run rust(axum framework) web server
run:
	@sqlx migrate run
	@cargo run

