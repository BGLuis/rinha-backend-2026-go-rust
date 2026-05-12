.PHONY: build up down restart logs smoke test docker-push docker-push-lb docker-push-all

DOCKER_COMPOSE = docker-compose
K6_IMAGE = grafana/k6
PWD = $(shell pwd)
API_IMAGE = luiis001/rinha-api-2026:latest
LB_IMAGE = luiis001/rinha-lb-2026:latest

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
	touch "$(PWD)/test/results.json" && chmod 666 "$(PWD)/test/results.json"
	docker run --rm --network host -u $(shell id -u):$(shell id -g) -v "$(PWD)/test:/test" -i $(K6_IMAGE) run /test/test.js

docker-push-api:
	docker build -t $(API_IMAGE) .
	docker push $(API_IMAGE)

docker-push-lb:
	docker build -f Dockerfile.lb -t $(LB_IMAGE) .
	docker push $(LB_IMAGE)

docker-push-all: docker-push-api docker-push-lb

run-all: restart
	sleep 5
	make smoke
