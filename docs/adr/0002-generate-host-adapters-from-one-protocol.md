# Generate host adapters from one protocol

Teamx defines the grill-with-docs workflow once as a host-neutral protocol and deterministically generates its OpenCode and DSH adapters. Generated adapters are committed and checked for drift because independently maintained prompts had already diverged, while requiring build-time-only generation would make clean checkouts and published packages less reliable.
