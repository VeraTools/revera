#!/usr/bin/env python3
"""Generate the M* model-screening configs (eval/configs/M*.yaml).

All configs share the same validator (glm-5.3-flash @ high via relay.fast)
so the comparison isolates the investigator/scout side. Reasoning `max` is
passed through only for gpt-5* models; other openai-chat models clamp to
`high` (see src/provider/openai_chat.rs), which is intentional here.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))

RELAY = "https://relay.fast/v1"
OPENCODE = "https://opencode.ai/zen/go/v1"


def relay(model, effort, max_out=6000):
    return f"""    protocol: openai-chat
    base_url: {RELAY}
    api_key_env: RELAY_FAST_API_KEY
    model: {model}
    max_output_tokens: {max_out}
    temperature: 0.2
    reasoning: {effort}
"""


def muse(max_out=6000):
    return f"""    protocol: openai-responses
    base_url: {OPENCODE}
    api_key_env: OPENCODE_GO_API_KEY
    model: muse-spark-1.3-contributor
    max_output_tokens: {max_out}
    temperature: 0.2
    reasoning: medium
"""


VALIDATOR = relay("glm-5.3-flash", "high", 4000)

VERA = """vera:
  executable: vera
  version: "1.4.1"
  backend: api
  embedding:
    base_url: https://openrouter.ai/api/v1
    model: qwen/qwen3-embedding-8b
    api_key_env: OPENROUTER_API_KEY
  reranker:
    base_url: https://openrouter.ai/api/v1
    model: qwen/qwen3-reranker-8b
    api_key_env: OPENROUTER_API_KEY
"""

BUDGET = """budget:
  run_max_seconds: 600
  agent_max_seconds: 300
  agent_max_tool_calls: 30
  run_max_requests: 120
"""


def baseline(investigator):
    return (
        "review:\n  strategy: baseline\n  publish: dry-run\n  min_severity: low\n"
        + BUDGET
        + "models:\n  investigator:\n"
        + investigator
        + "  validator:\n"
        + VALIDATOR
        + VERA
    )


def panel(scouts, concurrency):
    """scouts: list of (name, focus, route_yaml)."""
    focuses = "\n".join(f"    - {f}" for _, f, _ in scouts)
    s = (
        "review:\n  strategy: panel\n  publish: dry-run\n  min_severity: low\n"
        f"  concurrency: {concurrency}\n"
        + BUDGET
        + "panel:\n  focuses:\n"
        + focuses
        + "\n  scout_max_tool_calls: 30\n"
        + "models:\n  investigator:\n"
        + scouts[0][2]
        + "  validator:\n"
        + VALIDATOR
        + "  scouts:\n"
    )
    for name, focus, route in scouts:
        s += f"    - name: {name}\n      focus: {focus}\n"
        s += "".join("  " + line + "\n" for line in route.rstrip("\n").split("\n"))
    return s + VERA


GLM = relay("glm-5.3", "max")
FLASH = relay("glm-5.3-flash", "max")
TERRA = relay("gpt-5.6-terra", "max")
HY4 = relay("hy4", "high")
GEMINI = relay("gemini-3.8-flash", "high")
DEEPSEEK = relay("deepseek-v4.1-flash", "high")
GROK = relay("grok-4.6", "high")

CONFIGS = {
    "M1-glm": baseline(GLM),
    "M2-flash": baseline(FLASH),
    "M3-terra": baseline(TERRA),
    "M4-hy4": baseline(HY4),
    "M5-muse-opencode": baseline(muse()),
    # two independent samples of the same cheap model, identical focus
    "M6-panel-2xflash": panel(
        [("flash-a", "general", FLASH), ("flash-b", "general", FLASH)], 2
    ),
    # three complementary heterogeneous lanes
    "M7-panel-3het": panel(
        [
            ("glm", "general", GLM),
            ("gemini", "cross-file", GEMINI),
            ("deepseek", "data", DEEPSEEK),
        ],
        3,
    ),
    # three repeated cheap-model lanes with the same focuses as M7
    "M8-panel-3xflash": panel(
        [
            ("flash-a", "general", FLASH),
            ("flash-b", "cross-file", FLASH),
            ("flash-c", "data", FLASH),
        ],
        3,
    ),
    # five complementary lanes
    "M9-panel-5het": panel(
        [
            ("glm", "general", GLM),
            ("gemini", "cross-file", GEMINI),
            ("deepseek", "data", DEEPSEEK),
            ("grok", "concurrency", GROK),
            ("flash", "security", FLASH),
        ],
        5,
    ),
}

for name, body in CONFIGS.items():
    with open(os.path.join(HERE, f"{name}.yaml"), "w") as f:
        f.write(body)
    print(name)
