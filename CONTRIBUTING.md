# Contributing to JAM

Please note that this project is released with a [Contributor Code of Conduct](code-of-conduct.md). By participating in this project you agree to abide by its terms.

1. **Full names everywhere.** `actor_address`, never `pid` or `addr`. The only
   exceptions are ARM's own register names (`ESR_EL1`, `TTBR0_EL1`), which keep
   the specification's spelling so they stay searchable.
2. **One concept per file**, named for the concept. No `utils.rs`, no `common.rs`.
3. **`unsafe` lives only in `architecture/` and `memory/`.** Every block carries a
   `// SAFETY:` comment. `foundation` forbids `unsafe` outright.
4. **No panicking paths in the kernel.** No `unwrap`, `expect`, or unchecked
   indexing. Errors are typed and propagate.
5. **Magic numbers become named constants** in the module that owns them.
6. **Everything public is documented.** `missing_docs` is denied.
7. **Vocabulary is fixed** by `documentation/decisions/0001-vocabulary.md`.
   Do not reintroduce Erlang or POSIX terms.

1. [Fork it](https://github.com/your-username/jam/fork)
2. Create your feature branch (`git checkout -b my-new-feature`)
3. Commit your changes (`git commit -am 'Add some feature'`)
4. Push to the branch (`git push origin my-new-feature`)
5. Create a new Pull Request
