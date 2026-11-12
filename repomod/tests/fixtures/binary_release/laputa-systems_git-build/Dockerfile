FROM alpine:3.24.1@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b AS build

RUN apk add --no-cache binutils=2.45.1-r1 clang=22.0.0-r0 make=4.4.1-r4

WORKDIR /src
COPY . /src/

RUN make demo && mkdir /out && cp -a demo /out/
