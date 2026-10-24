# Local CLI

Review a committed branch with your own model keys. A local dry run calls
the models and prints the publication plan, but posts nothing to GitHub.

## Quickstart

1. Download the archive for your Linux architecture from
   [releases](https://github.com/VeraTools/revera/releases), verify it against
   the matching `.sha256` file, and put the extracted `revera` on `PATH`.
   If you have Rust installed, you can build from source instead:

   ```sh
   cargo install --git https://github.com/VeraTools/revera
   ```

2. Create `revera.yaml` in your repository root. Replace the endpoint and model
   ID with your provider's values:

   ```yaml
   models:
     investigator:
       protocol: openai-chat
       base_url: https://api.example.com/v1
       api_key_env: REVIEW_API_KEY
       model: your-model-id
   ```

   Validation uses this route in a fresh conversation. Vera is off by default;
   no embedding service or Vera binary is needed.
3. Set the named API key in your shell without saving it to the repository.
   In Bash, this avoids putting its value in shell history:

   ```sh
   read -rsp 'Model API key: ' REVIEW_API_KEY; printf '\n'
   export REVIEW_API_KEY
   revera doctor --config revera.yaml
   ```

   Doctor checks configuration and required credentials without calling your
   provider. Passing it does not prove that the endpoint or key works.
4. Add the [local ignore rules](#local-files-and-retrieval), commit the changes
   you want reviewed and ensure the working tree is clean. Fetch the base
   branch, then review:

   ```sh
   git fetch origin main
   revera review --repo . --base origin/main --head HEAD \
     --config revera.yaml --publish dry-run --out report.json
   ```

   Replace `origin/main` if your base branch has another name. The checked-out
   tree must match the reviewed head; uncommitted edits are not reviewed.

## Profiles and reports

Add `--profile quick` for a smaller budget or `--profile deep` for a larger
one. Both keep validation and the configured strategy. User-defined profiles
can override them. See [profiles](configuration.md#profiles).

The CLI exits `0` for `complete`, `2` for `partial`, and another code for
failure. A partial review with no findings is not a clean verdict. The JSON
report includes stop reasons, coverage gaps, findings, timing and model usage.
Without `--out`, it is written to `.revera/last-report.json`.

The report's `findings` list can contain rejected or uncertain candidates as
well as accepted findings. Check `validation_status`; only `accepted` findings
are publishable. A validator's `validated_fix` is a suggested remedy, not a
tested patch. See [finding details](how-it-works.md#findings).
The separate `carried` list contains previously published findings still open
from earlier reviews, excluding this run's findings. It keeps instructions and
fix requests available without changing accepted counts or posting them again.
Reports without this optional field are still supported.

An identical completed review can be reused. Add `--force` to run it again.

## Review guidance

Use `--guidance off|review|agents|skills` to override `review.guidance` (default
`off`). Guidance files and skills come from the base commit, never the PR head.
Append operator review notes with `--notes-file notes.md`; combined with
`review.notes`, the limit is 8000 characters. With `--event`, the notes file
must be outside the checkout, because the PR controls files inside it. Both investigators and validators
receive the same context. Changing the mode or notes invalidates a stored
review, but keeps the Vera index warm. Private external reviews ignore notes
and force guidance off. See [guidance settings](configuration.md#review).

## Local files and retrieval

Review state lives in `.revera/`. With Vera enabled, indexes use `.vera/`
and Vera's companion `.vera*` directories. Add these entries to `.gitignore`
before running, and ignore any custom report path too:

```gitignore
.revera/
.vera*/
report.json
```

For semantic search, install Vera and add a [`vera` block](configuration.md#vera).
`revera index --config revera.yaml` builds or refreshes the index without
reviewing. Revera uses its own Vera configuration and cache home rather than
your ambient Vera settings.

Optional [web search](configuration.md#web_search) sends queries to Exa.
Local reviews have unknown repository visibility, so enabling web tools also
requires trusted `allow_private: true`. Leave it off if outbound queries are
not acceptable.

To publish reviews, use the [Action](action.md) or [GitHub App](server.md).
The [configuration reference](configuration.md#command-line) lists common
commands. Run `revera --help` or `revera review --help` for all available flags.
