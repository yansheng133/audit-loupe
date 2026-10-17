# Build and runtime images from SUSE BCI. Build on the target architecture
# (x86-64 or ARM64); there is no cross-compilation step.
FROM registry.suse.com/bci/rust:latest AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM registry.suse.com/bci/bci-micro:latest
COPY --from=build /src/target/release/audit-loupe /usr/local/bin/audit-loupe
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/audit-loupe"]
