.PHONY: build up down restart logs smoke test

DOCKER_COMPOSE = docker-compose
K6_IMAGE = grafana/k6
PWD = $(shell pwd)

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
	docker run --rm --network host -v "$(PWD)/test:/test" -i $(K6_IMAGE) run /test/test.js

run-all: restart
	sleep 5
	make smoke
