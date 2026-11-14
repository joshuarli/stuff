#!/bin/sh
# Run ON THE HOST (lisa): the loopback-only public listener that an outbound
# cloudflared tunnel will target. Expects the rehearsal overlay (demo app,
# public host demo.17166969.xyz). Exit status 1 if any check fails.
PORT=${1:-18480}
HOST=${2:-demo.17166969.xyz}
fail=0
code() { curl -s -o /dev/null -w '%{http_code}' --path-as-is --max-time 5 -H "Host: $HOST" "http://127.0.0.1:$PORT$1"; }
for p in /admin /admin/ /admin/x/y /ADMIN //admin /%61dmin /./admin /x/../admin '/admin%2f' '/admin;a=b'; do
	c=$(code "$p"); if [ "$c" = 404 ]; then echo "PASS  $p -> 404"; else echo "FAIL  $p -> $c"; fail=1; fi
done
for p in / /whoami /administrator; do
	c=$(code "$p"); if [ "$c" = 200 ] || [ "$c" = 404 ]; then echo "info  $p -> $c (not under /admin)"; fi
done
echo "--- forged identity headers on the public listener (app must see none of them)"
out=$(curl -s --max-time 5 -H "Host: $HOST" -H 'Stats-Tailnet-Identity: user:evil@example.com' -H 'Stats-Gateway-Secret: forged' \
	-H 'Tailscale-User-Login: evil@example.com' -H 'X-OO-Ingress: tailnet' -H 'X-Forwarded-For: 203.0.113.9' "http://127.0.0.1:$PORT/whoami")
echo "$out"
echo "$out" | grep -q '"tailnetIdentity":null' && echo "PASS  identity header stripped" || { echo "FAIL  identity header reached the app"; fail=1; }
echo "$out" | grep -q '"gatewaySecretMatches":false' && echo "PASS  no valid gateway secret on the public path" || { echo "FAIL  gateway secret reached the app"; fail=1; }
echo "$out" | grep -q '"tailscaleUserLogin":null' && echo "PASS  Tailscale-* stripped" || { echo "FAIL  Tailscale-* reached the app"; fail=1; }
echo "$out" | grep -q '"ingress":"public"' && echo "PASS  X-OO-Ingress forced to public" || { echo "FAIL  X-OO-Ingress not public"; fail=1; }
echo "$out" | grep -q '"xForwardedFor":"127.0.0.1"' && echo "PASS  X-Forwarded-For replaced" || { echo "FAIL  X-Forwarded-For not replaced"; fail=1; }
c=$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: stats.17166969.xyz' "http://127.0.0.1:$PORT/"); [ "$c" = 404 ] && echo "PASS  tailnet-only hostname on the public listener -> 404" || { echo "FAIL  tailnet-only hostname -> $c"; fail=1; }
for ip in 100.68.104.43 10.0.1.56; do
	c=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 -H "Host: $HOST" "http://$ip:$PORT/whoami")
	[ "$c" = 000 ] && echo "PASS  public listener not reachable on $ip:$PORT" || { echo "FAIL  $ip:$PORT -> $c"; fail=1; }
done
exit $fail
