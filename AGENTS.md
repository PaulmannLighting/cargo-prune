# Rust development

- Format Rust code with `cargo +nightly fmt` and verify it with `cargo +nightly fmt --check`.
- Check Rust code with `cargo clippy --all-features` and resolve any diagnostics.

## Supply chain audits

- Manage dependency audits with `cargo vet`. You may import audits from Google and Mozilla.
- Never add exemptions.
- Resolve audit gaps without user interaction. Review the dependency's source diff from an audited version, or review the full source when no audited baseline is available. Record the findings in `supply-chain/audits.toml` and mark each audit you perform as `AI-assisted`.
- After updating audits, run `cargo vet regenerate unpublished`, `cargo vet prune`, and `cargo vet`.

## Documentation

- Never create `ARCHITECTURE.md` or any case variant of that filename in this repository.
