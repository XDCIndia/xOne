# MDBX capacity and growth (`--db.max-size`, `--db.growth-step`)

`chain.mdbx` (under `<datadir>/xone`) is an MDBX memory map with an upper bound and a growth step.

| flag | default | bounds |
|---|---|---|
| `--db.max-size` | `1TB` | `16MB` ..= `8TB` (MDBX's limit with 4 KiB pages) |
| `--db.growth-step` | `64MB` | `1MB` ..= `--db.max-size` |

Sizes are bytes or a number with `KB`/`MB`/`GB`/`TB` (also `KiB`..`TiB`); every suffix is a power
of 1024. The TOML keys are `[sync] db_max_size` and `db_growth_step`, in bytes; `xone dumpconfig`
prints them.

- Startup validates both values and refuses an existing `mdbx.dat` larger than `--db.max-size`,
  naming the file and the flag, before the database opens.
- Raising `--db.max-size` on an existing database keeps every row: restart with the new value.
- When the map is full a write fails with an `MDBX_MAP_FULL` error that names `--db.max-size`; the
  failed transaction aborts and committed data is intact. Restart with a larger maximum.
- Durability (`SyncMode`, group commit) is independent of the geometry.
- A larger step means fewer remaps under heavy import; a smaller one means a smaller file on disk.
