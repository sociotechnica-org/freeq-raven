.PHONY: bootstrap start stop restart status logs check

bootstrap:
	./bin/freeq-raven-bootstrap

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
