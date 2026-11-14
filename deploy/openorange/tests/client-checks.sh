#!/bin/sh
# Rehearsal checks that must run from a tailnet client (not from the host).
#   client-checks.sh [HOST_TAILNET_IP] [HOST_LAN_IP]
# Expects the rehearsal overlay: a demo Worker answers stats.17166969.xyz.
# Prints every command and its observed output; exit status 1 if any check fails.
TS=${1:-100.68.104.43}
LAN=${2:-10.0.1.56}
CURL="curl -sk --max-time 8"
fail=0
check() { # name, condition-result
	if [ "$2" = 0 ]; then echo "PASS  $1"; else echo "FAIL  $1"; fail=1; fi
}
run() { echo "\$ $*" >&2; "$@"; }

echo "== 1. request gets through, identity forwarded from the sidecar"
out=$(run $CURL --resolve stats.17166969.xyz:443:$TS https://stats.17166969.xyz/whoami); echo "$out"
echo "$out" | grep -q '"tailnetIdentity":"user:' ; check "identity header forwarded as user:<login>" $?
echo "$out" | grep -q '"gatewaySecretMatches":true' ; check "gateway secret reached the app and matches" $?

echo "== 2. forged headers are stripped / overwritten"
out=$(run $CURL --resolve stats.17166969.xyz:443:$TS https://stats.17166969.xyz/whoami \
	-H 'Stats-Tailnet-Identity: user:evil@example.com' -H 'Stats-Gateway-Secret: forged-secret' \
	-H 'Tailscale-User-Login: evil@example.com' -H 'Tailscale-User-Name: Evil' -H 'X-Forwarded-For: 203.0.113.9' \
	-H 'X-Forwarded-Host: evil.example' -H 'X-OO-Peer: 100.64.0.9:1' -H 'Forwarded: for=203.0.113.9'); echo "$out"
echo "$out" | grep -q 'evil' ; [ $? -ne 0 ]; check "no forged identity value reaches the app" $?
echo "$out" | grep -q '"tailscaleUserLogin":null' ; check "client Tailscale-User-Login stripped" $?
echo "$out" | grep -q '"xForwardedFor":"100\.' ; check "X-Forwarded-For replaced by the true tailnet peer" $?
echo "$out" | grep -q '"gatewaySecretMatches":true' ; check "forged Stats-Gateway-Secret replaced by the real per-app secret" $?

echo "== 3. tansu site block (native upstream 127.0.0.1:3001)"
out=$(run $CURL -w ' [http %{http_code}]' --resolve tansu.17166969.xyz:443:$TS https://tansu.17166969.xyz/api/v2/health); echo "$out"
echo "$out" | grep -q 'http 200' ; check "tansu /api/v2/health answers through the ingress" $?

echo "== 4. engine / store / app / admin ports are not reachable on the tailnet or LAN address"
for ip in $TS $LAN; do
	for port in 6420 6421 6422 6430 7070 7071 8080 80 18480; do
		if nc -z -G 3 $ip $port 2>/dev/null; then echo "OPEN  $ip:$port"; fail=1; else echo "closed $ip:$port"; fi
	done
done
nc -z -G 3 $TS 443 2>/dev/null; check "$TS:443 (ingress) is the one open port" $?
if nc -z -G 3 $LAN 443 2>/dev/null; then echo "FAIL  LAN $LAN:443 open"; fail=1; else echo "PASS  LAN $LAN:443 closed (ingress bound to tailnet address only)"; fi

echo "== 5. same hostname on the tailnet listener serves /admin (public listener does not; see public-listener-checks.sh)"
out=$(run $CURL -w ' [http %{http_code}]' --resolve demo.17166969.xyz:443:$TS https://demo.17166969.xyz/admin/x); echo "$out"
echo "$out" | grep -q 'http 200' ; check "tailnet listener serves /admin/* with identity" $?
out=$(run $CURL --resolve demo.17166969.xyz:443:$TS https://demo.17166969.xyz/whoami); echo "$out"
echo "$out" | grep -q '"ingress":"tailnet"' ; check "app sees X-OO-Ingress: tailnet" $?

echo "== 6. TLS identity of the listener"
echo | openssl s_client -connect $TS:443 -servername stats.17166969.xyz 2>/dev/null | openssl x509 -noout -subject -issuer -ext subjectAltName 2>/dev/null

exit $fail
