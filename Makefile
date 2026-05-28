.PHONY: build up down restart restart-clean logs smoke test heavy-test test-precision test-thermal test-sustained test-saturation test-spike all-tests run-all docker-push-api docker-push-lb docker-push-all

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

restart-clean:
	$(DOCKER_COMPOSE) down
	$(DOCKER_COMPOSE) up -d --build
	@echo "Waiting for stack to start..."
	sleep 3
	@echo "Stack is ready."

logs:
	$(DOCKER_COMPOSE) logs -f

smoke:
	docker-compose -f test/docker-compose.yml --profile smoke up

test:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results.json" && chmod 666 "$(PWD)/test/results.json"
	docker-compose -f test/docker-compose.yml --profile test up

heavy-test:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_heavy.json" && chmod 666 "$(PWD)/test/results_heavy.json"
	docker-compose -f test/docker-compose.yml --profile heavy up

test-precision:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_precision.json" && chmod 666 "$(PWD)/test/results_precision.json"
	docker-compose -f test/docker-compose.yml --profile precision up

test-thermal:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_thermal.json" && chmod 666 "$(PWD)/test/results_thermal.json"
	docker-compose -f test/docker-compose.yml --profile thermal up

test-sustained:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_sustained.json" && chmod 666 "$(PWD)/test/results_sustained.json"
	docker-compose -f test/docker-compose.yml --profile sustained up

test-saturation:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_saturation.json" && chmod 666 "$(PWD)/test/results_saturation.json"
	docker-compose -f test/docker-compose.yml --profile saturation up

test-spike:
	mkdir -p "$(PWD)/test"
	touch "$(PWD)/test/results_spike.json" && chmod 666 "$(PWD)/test/results_spike.json"
	docker-compose -f test/docker-compose.yml --profile spike up

docker-push-api:
	docker build -t $(API_IMAGE) .
	docker push $(API_IMAGE)

docker-push-lb:
	docker build -f Dockerfile.lb -t $(LB_IMAGE) .
	docker push $(LB_IMAGE)

docker-push-all: docker-push-api docker-push-lb

all-tests:
	@echo "Running ALL tests in sequence from easiest to hardest..."
	$(MAKE) restart-clean
	$(MAKE) smoke
	$(MAKE) restart-clean
	$(MAKE) test-precision
	$(MAKE) restart-clean
	$(MAKE) test-thermal
	$(MAKE) restart-clean
	$(MAKE) test
	$(MAKE) restart-clean
	$(MAKE) test-sustained
	$(MAKE) restart-clean
	$(MAKE) test-saturation
	$(MAKE) restart-clean
	$(MAKE) test-spike
	$(MAKE) restart-clean
	$(MAKE) heavy-test

run-all: all-tests
