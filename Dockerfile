FROM php:8.5-zts-trixie AS builder

RUN apt-get update && apt-get install -y \
    curl \
    clang \
    libclang-dev \
    build-essential

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y

ENV PATH="/root/.cargo/bin:$PATH"

WORKDIR /app

COPY . .

RUN LIBRARY_PATH=/usr/local/lib cargo build --release --bin ferrumphp

FROM php:8.5-zts-trixie

COPY --from=builder /app/target/release/ferrumphp /usr/local/bin/ferrumphp

RUN cp "$PHP_INI_DIR/php.ini-development" "$PHP_INI_DIR/php.ini"

WORKDIR /app

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/ferrumphp"]

