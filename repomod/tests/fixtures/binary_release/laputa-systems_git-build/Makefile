CC ?= clang
CFLAGS ?= -O3 -fstack-protector-strong
LDFLAGS ?= -static-pie

demo: src/demo/demo.c
	$(CC) $(CFLAGS) src/demo/demo.c -o demo $(LDFLAGS)

clean:
	rm -f demo
