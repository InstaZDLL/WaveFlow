# shellcheck shell=bash
# R2_BUCKET and R2_ENDPOINT are set here for the scripts that source this.
# shellcheck disable=SC2034
#
# Shared by the scripts that write to the R2 bucket: publish-r2.sh (a
# release) and publish-plugins-r2.sh (the plugin store). Sourced, never
# run: it defines functions and one message, and sets nothing else
# until a function is called.
#
# Environment read:
#   R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY  an R2 API token's S3 keys
#   R2_ACCOUNT_ID                          the Cloudflare account id, or its
#                                          S3 endpoint https://<id>.r2.cloudflarestorage.com
#   R2_BUCKET                              the bucket's name
#   R2_DRY_RUN=1                           r2_run prints instead of running

R2_HALF_CONFIGURED="R2 mirror half configured: set all four of R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ACCOUNT_ID, R2_BUCKET, or none"

# "absent" (none of the four secrets), "configured" (all four) or
# "partial". Never prints a value.
r2_state() {
  local set=0
  for v in R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_ACCOUNT_ID R2_BUCKET; do
    [ -n "${!v:-}" ] && set=$((set + 1))
  done
  case "$set" in
    0) echo absent ;;
    4) echo configured ;;
    *) echo partial ;;
  esac
}

# The account id, or the S3 endpoint the dashboard shows right beside it,
# https://<id>.r2.cloudflarestorage.com: the secret has been set to the second
# once already (on the server's repository), and the upload then failed on
# DNS with nothing saying why. Prints the bare id; anything else is refused
# by name.
r2_account() {
  local account="${R2_ACCOUNT_ID#https://}"
  account="${account%%/*}"
  account="${account%%.*}"
  case "$account" in
    "" | *[!0-9a-f]*) ;;
    *) if [ "${#account}" -eq 32 ]; then echo "$account"; return 0; fi ;;
  esac
  echo "::error::R2_ACCOUNT_ID is neither an account id nor https://<id>.r2.cloudflarestorage.com" >&2
  return 1
}

# The `--check` mode both scripts offer: "configured" or "absent" on
# stdout, an error for half a configuration or a wrong account id.
r2_check() {
  local state
  state="$(r2_state)"
  if [ "$state" = "partial" ]; then
    echo "::error::$R2_HALF_CONFIGURED" >&2
    return 1
  fi
  # Checked here too, so a wrong id stops the job before anything is
  # fetched rather than at the first upload.
  if [ "$state" = "configured" ]; then r2_account >/dev/null || return 1; fi
  echo "$state"
}

# Exports what the AWS CLI reads and sets R2_ENDPOINT. With no secret
# set (a dry run), the bucket and account become placeholders: a dry run
# needs names to print, not credentials.
r2_aws_env() {
  local state="$1"
  export AWS_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:-}"
  export AWS_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:-}"
  export AWS_DEFAULT_REGION=auto
  # AWS CLI 2.23 started sending CRC checksums R2 does not take on every
  # request; Cloudflare's guidance is to send them only where required.
  export AWS_REQUEST_CHECKSUM_CALCULATION=when_required
  export AWS_RESPONSE_CHECKSUM_VALIDATION=when_required
  if [ "$state" = "absent" ]; then
    R2_ACCOUNT_ID="<account-id>"
    R2_BUCKET="<bucket>"
    R2_ENDPOINT="https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com"
  else
    local account
    account="$(r2_account)" || return 1
    R2_ENDPOINT="https://$account.r2.cloudflarestorage.com"
  fi
}

# Runs its arguments, or prints them under R2_DRY_RUN=1. The command
# line holds the bucket and the endpoint, never a key: those travel in
# the environment.
r2_run() {
  if [ "${R2_DRY_RUN:-0}" = "1" ]; then
    printf '[dry-run]'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}
