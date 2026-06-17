.PHONY: bootstrap build start stop restart status logs check test

bootstrap:
	./bin/freeq-raven-bootstrap

build: bootstrap

start:
	./bin/freeq-raven-start

stop:
	./bin/freeq-raven-stop

restart: stop start

status:
	./bin/freeq-raven-status

logs:
	./bin/freeq-raven-logs

check:
	./bin/freeq-raven-bootstrap --check
	./tests/raven-claude-runner-smoke.sh

test:
	cargo test -p freeq-raven --lib
	cargo test -p freeq-raven identity --test identity_test
