# my_project Substreams modules

This package was initialized via `substreams init`, using the `sol-minimal` template.

## Usage

```bash
substreams build
substreams auth
substreams gui
```

## Modules

### `map_filtered_transactions`

This module retrieves Solana transactions filtered by one or several Program IDs.
You will only receive transactions containing the specified Program IDs).

**NOTE:** Transactions containing voting instructions will NOT be present.
