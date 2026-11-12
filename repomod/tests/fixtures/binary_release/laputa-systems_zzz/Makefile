TARGET ?= x86_64-linux-musl
CPU    ?= baseline
FLAGS   = -OReleaseSmall -fstrip -fno-unwind-tables

zzz: zzz.zig
	zig build-exe $< -target $(TARGET) -mcpu=$(CPU) $(FLAGS) --name $@

PREFIX ?= /usr/local

install: zzz
	install -Dm 4755 $< $(DESTDIR)$(PREFIX)/bin/$<

clean:
	rm -f zzz
