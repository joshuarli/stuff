import hashlib
import tempfile
import urllib.error
import urllib.parse
import urllib.request

from . import VERSION
from .crate_file import ScanLimits, scan_crate_staged
from .errors import InputError, IntegrityError, LimitError, NetworkError
from .model import IndexEntry
from .names import shard_for_name
from .wirejson import loads

LOOPBACK_HOSTS = ("127.0.0.1", "localhost", "::1")
READ_CHUNK = 65536
RESPONSE_SLACK = 10 * 1024 * 1024
DL_PLACEHOLDERS = ("{crate}", "{version}", "{prefix}", "{lowerprefix}", "{sha256-checksum}")


class _NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def check_url_security(url):
    try:
        parts = urllib.parse.urlsplit(url)
        host = parts.hostname
        _ = parts.port
    except ValueError:
        raise InputError(f"invalid URL (HTTPS required unless loopback): {url}") from None
    if not parts.scheme or not host:
        raise InputError(f"invalid URL (HTTPS required unless loopback): {url}")
    if parts.username is not None or parts.password is not None:
        raise InputError(f"credentials in URL forbidden: {url}")
    if parts.scheme == "https":
        return
    if parts.scheme == "http" and host in LOOPBACK_HOSTS:
        return
    raise InputError(f"insecure URL forbidden (HTTPS required unless loopback): {url}")


class UpstreamClient:
    def __init__(self, index_url, limits, spill_dir=None):
        check_url_security(index_url)
        self.index_url = index_url
        self.limits = limits
        self.spill_dir = spill_dir
        self._opener = urllib.request.build_opener(_NoRedirects)

    def _open(self, url, what):
        check_url_security(url)
        request = urllib.request.Request(url, headers={"User-Agent": f"mycrates/{VERSION}"})
        try:
            return self._opener.open(request, timeout=self.limits.request_timeout_secs)
        except urllib.error.HTTPError as error:
            if 300 <= error.code < 400:
                return error
            raise NetworkError(f"failed to {what} {url}: HTTP {error.code}") from None
        except (urllib.error.URLError, OSError, ValueError) as error:
            raise NetworkError(f"failed to {what} {url}: {error}") from error

    def _follow(self, url, what):
        current = url
        for _ in range(self.limits.redirects + 1):
            response = self._open(current, what)
            if 300 <= response.status < 400:
                location = response.headers.get("Location")
                response.close()
                if not location:
                    raise NetworkError(f"redirect without Location from {current}")
                current = urllib.parse.urljoin(current, location)
                continue
            if not 200 <= response.status < 300:
                response.close()
                raise NetworkError(f"failed to {what} {current}: HTTP {response.status}")
            return current, response
        raise NetworkError(f"too many redirects fetching {url}")

    def get(self, url):
        final, response = self._follow(url, "fetch")
        limit = self.limits.max_compressed + RESPONSE_SLACK
        body = bytearray()
        try:
            while True:
                chunk = response.read(READ_CHUNK)
                if not chunk:
                    break
                body.extend(chunk)
                if len(body) > limit:
                    raise LimitError("response exceeds size limit")
        except OSError as error:
            raise NetworkError(f"stream read error: {error}") from error
        finally:
            response.close()
        check_url_security(final)
        return bytes(body)

    def fetch_dl_template(self):
        body = self.get(f"{self.index_url.rstrip('/')}/config.json")
        try:
            document = loads(body.decode("utf-8"))
        except (InputError, UnicodeDecodeError) as error:
            raise IntegrityError(f"invalid config.json from upstream: {error}") from error
        dl = document.get("dl") if isinstance(document, dict) else None
        if not isinstance(dl, str):
            raise IntegrityError("missing dl in upstream config.json")
        return dl

    def expand_dl_url(self, template, name, vers, cksum):
        shard = shard_for_name(name.lower())
        if shard is None:
            raise InputError(f"crate name is not ASCII: {name}")
        prefix = shard.rpartition("/")[0]
        if not any(p in template for p in DL_PLACEHOLDERS):
            template = f"{template.rstrip('/')}/{{crate}}/{{version}}/download"
        expanded = (
            template.replace("{crate}", name)
            .replace("{version}", vers)
            .replace("{prefix}", prefix)
            .replace("{lowerprefix}", prefix.lower())
            .replace("{sha256-checksum}", cksum)
        )
        if "{" in expanded and "}" in expanded:
            raise InputError(f"unsupported placeholder in dl template: {expanded}")
        check_url_security(expanded)
        return expanded

    def fetch_index_entry(self, name, vers):
        shard = shard_for_name(name.lower())
        if shard is None:
            raise InputError(f"crate name is not ASCII: {name}")
        try:
            text = self.get(f"{self.index_url.rstrip('/')}/{shard}").decode("utf-8")
        except UnicodeDecodeError as error:
            raise NetworkError(f"failed to read index response: {error}") from error
        for line in text.split("\n"):
            line = line.strip()
            if not line:
                continue
            try:
                entry = IndexEntry.from_obj(loads(line))
            except InputError:
                continue
            if entry.name == name and entry.vers == vers:
                return line, entry
        raise InputError(f"crate {name} version {vers} not found in upstream index")

    def download_crate(self, url, expected_checksum, name, vers):
        final, response = self._follow(url, "download crate from")
        spill = tempfile.NamedTemporaryFile(dir=self.spill_dir, prefix=".download.")
        try:
            digest = hashlib.sha256()
            counted = 0
            try:
                while True:
                    chunk = response.read(READ_CHUNK)
                    if not chunk:
                        break
                    counted += len(chunk)
                    if counted > self.limits.max_compressed:
                        raise LimitError("downloaded crate exceeds max_compressed limit")
                    digest.update(chunk)
                    spill.write(chunk)
            except OSError as error:
                raise NetworkError(f"stream read error: {error}") from error
            finally:
                response.close()
            spill.flush()
            check_url_security(final)
            actual = digest.hexdigest()
            if actual != expected_checksum:
                raise IntegrityError(
                    f"download checksum mismatch for {name} {vers}: expected {expected_checksum}, got {actual}"
                )
            scanned = scan_crate_staged(
                spill,
                counted,
                self.limits.max_compressed,
                expected_checksum,
                name,
                vers,
                ScanLimits.from_config(self.limits),
            )
            if scanned.checksum != expected_checksum:
                raise IntegrityError(f"scanned archive checksum mismatch for {name} {vers}")
        except BaseException:
            spill.close()
            raise
        return spill
