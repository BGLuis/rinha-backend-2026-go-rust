.PHONY: build up down restart logs smoke test docker-push

DOCKER_COMPOSE = docker-compose
K6_IMAGE = grafana/k6
PWD = $(shell pwd)
IMAGE_NAME = luiis001/rinha-api-2026:latest

build:
	$(DOCKER_COMPOSE) build

up:
	$(DOCKER_COMPOSE) up -d

down:
	$(DOCKER_COMPOSE) down

restart:
	$(DOCKER_COMPOSE) down && $(DOCKER_COMPOSE) up -d --build

logs:
	$(DOCKER_COMPOSE) logs -f

smoke:
	docker run --rm --network host -i $(K6_IMAGE) run - <test/smoke.js

test:
	docker run --rm --network host -u $(shell id -u):$(shell id -g) -v "$(PWD)/test:/test" -i $(K6_IMAGE) run /test/test.js

docker-push:
	docker build -t $(IMAGE_NAME) .
	docker push $(IMAGE_NAME)

run-all: restart
	sleep 5
	make smoke
