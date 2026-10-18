# Web evidence: raw paired run, 2026-10-17

One repetition per arm/case; GLM `glm-5.3` investigator and `glm-5.3-flash`
validator, lexical repository tools, identical prompts and model/tool/time
budgets. The configs differ only in `web_search.enabled`. Web-on used free
hosted Exa (no API key). Truth is defined by `build-corpus-web.sh` and remains
untracked in each generated repository. Scored JSON rows are appended at
`eval/results.jsonl:310-319`; reports are in ignored `eval/reports/WEB-*.json`.

Binary SHA-256 used for these runs:
`cdca0700cf95d73736f878ed768c566dd2d2f78c90190559e7db44b911a43854`.
Subsequent changes record cancelled tool calls as errors and classify timeouts
while reading an HTTP body; they were offline-tested, not rerun in this table.

| arm | corpus | TP | FP | uncertain | rejected | tool calls | web attempts | seconds | tokens | status |
|---|---|---|---|---|---|---|---|---|---|---|
| off | contract | 0 | 0 | 0 | 0 | 8 | 0 | 29.7 | 15341 | partial |
| on | contract | 1 | 0 | 0 | 0 | 18 | 7 | 288.7 | 85434 | complete |
| off | version-change | 0 | 0 | 0 | 0 | 5 | 0 | 16.7 | 9290 | complete |
| on | version-change | 1 | 0 | 0 | 0 | 11 | 3 | 173.2 | 37903 | complete |
| off | wrong-version | 0 | 0 | 0 | 0 | 7 | 0 | 107.8 | 23134 | partial |
| on | wrong-version | 0 | 0 | 0 | 0 | 9 | 2 | 169.6 | 35691 | partial |
| off | clean-libraries | 0 | 0 | 0 | 0 | 2 | 0 | 6.0 | 4246 | complete |
| on | clean-libraries | 0 | 0 | 0 | 0 | 1 | 0 | 7.0 | 4632 | complete |
| off | injected-leak | 0 | 0 | 0 | 0 | 4 | 0 | 14.7 | 7211 | complete |
| on | injected-leak | 0 | 0 | 0 | 0 | 2 | 0 | 13.9 | 5416 | complete |

Tokens are prompt plus completion, not an estimated cost. Web attempts are
actual outbound HTTP attempts; rejected tool invocations are included only in
tool calls. External-call prices are not estimated; hosted Exa needs no key.
TP/FP are the existing harness's automated truth matches, not independent human
adjudication. No run failed; the three partial reasons are recorded below.

- Off/contract: investigator explicitly left the external response contract unchecked.
- Off/wrong-version and on/wrong-version: investigator stopped with `NoTerminalCall`.
- On/version-change: telemetry contains three `web_search` requests and **zero
  `web_read` calls**, despite the exact-version/read-before-accept prompt; the
  reports contain no transcripts with which to establish another verification path.
- On/contract: three search attempts and four read attempts (one read error);
  six search and six read tool invocations in total (including rejections).
- Both clean-libraries and injected-leak arms made zero web attempts.

The REST and hosted search/read live smokes both passed separately. Defaults
remain off. These are raw observations, not a quality or release recommendation.

Reproduce after configuring model credentials:
`TMPDIR="$HOME/.local/tmp" CARGO_BUILD_JOBS=5 bash eval/run-web.sh 1`
with `EXA_API_KEY` unset for free hosted Exa, or set for REST. Never publish keys.
