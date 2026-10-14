#!/usr/bin/env bash
# Routes supported GitHub events to review or index mode for action.yml.
#
#   action-mode.sh EVENT_NAME
set -euo pipefail

event=${1:?usage: action-mode.sh EVENT_NAME}
case "$event" in
  pull_request|pull_request_target) echo review ;;
  push|schedule|workflow_dispatch) echo index ;;
  *)
    echo "revera: unsupported event: $event (supported: pull_request, pull_request_target, push, schedule, workflow_dispatch)" >&2
    exit 1
    ;;
esac
