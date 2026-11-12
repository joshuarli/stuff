release:
	$(call cargo,release-static) build --profile dist --locked \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
