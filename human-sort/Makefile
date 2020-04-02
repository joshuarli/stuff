all: humansort natsort

humansort: main.go
	go build -o $@ main.go

natsort:
	go build -o $@ natsort.go

clean:
	rm -f humansort natsort
