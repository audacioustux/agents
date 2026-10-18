#!/bin/bash
# Registers a runner without its access token ever being in the container's
# environment. The image's own ACCESS_TOKEN option keeps the token in PID 1's
# initial environment, readable from /proc/1/environ and `docker inspect` for
# the container's whole life, and its UNSET_CONFIG_VARS only unsets it in
# bash's own table. Here the token is read once from a root-only file,
# exchanged for the one-hour registration token, and the image is started with
# that alone.
set -euo pipefail

secret_file=/run/secrets/runner_access_token

# A credential variable left in the service's environment would put a token
# straight back where this script exists to keep it out of.
for name in ACCESS_TOKEN RUNNER_TOKEN APP_PRIVATE_KEY; do
  if [[ -n "${!name+x}" ]]; then
    echo "refusing to start: ${name} is set in the environment; the access token belongs in ${secret_file}" >&2
    exit 1
  fi
done

if [[ "$(stat -c '%u:%a' "${secret_file}")" != "0:400" ]]; then
  echo "refusing to start: ${secret_file} must be owned by root with mode 0400" >&2
  exit 1
fi

RUNNER_TOKEN=$(ACCESS_TOKEN=$(<"${secret_file}") bash /token.sh | jq -r .token)
if [[ -z "${RUNNER_TOKEN}" || "${RUNNER_TOKEN}" == "null" ]]; then
  echo "refusing to start: GitHub returned no registration token for the access token in ${secret_file}" >&2
  exit 1
fi
export RUNNER_TOKEN

exec /entrypoint.sh "$@"
