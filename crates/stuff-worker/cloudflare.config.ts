import { bindings, defineConfig, exports } from "cf/config";
import * as entrypoint from "./entry.ts" with { type: "cf-worker" };

export default defineConfig(() => ({
	worker: {
		name: "stuff",
		compatibilityDate: "2026-10-30",
		entrypoint,
		exports: { PublicationCoordinator: exports.durableObject({ storage: "sqlite" }) },
		cache: {
			enabled: true,
		},
		observability: {
			enabled: true,
			logs: {
				enabled: true,
				invocationLogs: true,
				headSamplingRate: 0.25,
			},
			traces: {
				enabled: true,
				headSamplingRate: 0.1,
			},
		},
		domains: [
			"crates.17166969.xyz",
			"crates-blobs.17166969.xyz",
			"npm.17166969.xyz",
			"pypi.17166969.xyz",
			"stuff.17166969.xyz",
			"oci.17166969.xyz",
			"oci-blobs.17166969.xyz",
			"mirror.17166969.xyz",
		],
		env: {
			STUFF_ROUTES: bindings.text("[{\"host\":\"crates.17166969.xyz\",\"bucket_binding\":\"MYCRATES\",\"key_prefix\":\"\"},{\"host\":\"crates-blobs.17166969.xyz\",\"bucket_binding\":\"MYCRATES\",\"key_prefix\":\"\"},{\"host\":\"npm.17166969.xyz\",\"bucket_binding\":\"MYNPM\",\"key_prefix\":\"\"},{\"host\":\"pypi.17166969.xyz\",\"bucket_binding\":\"PYPI\",\"key_prefix\":\"\"},{\"host\":\"oci.17166969.xyz\",\"bucket_binding\":\"OCI\",\"key_prefix\":\"\"},{\"host\":\"mirror.17166969.xyz\",\"bucket_binding\":\"MIRROR\",\"key_prefix\":\"\"}]"),
			STUFF_CARGO_BLOB_HOST: bindings.text("crates-blobs.17166969.xyz"),
			STUFF_OCI_BLOB_HOST: bindings.text("oci-blobs.17166969.xyz"),
			STUFF_UNIFIED_HOST: bindings.text("stuff.17166969.xyz"),
			STUFF_OCI_HOST: bindings.text("oci.17166969.xyz"),
			STUFF_OCI_BLOB_ORIGIN: bindings.text("https://oci-blobs.17166969.xyz"),
			MYCRATES: bindings.r2({
				name: "stuff-mycrates",
			}),
			MYNPM: bindings.r2({
				name: "stuff-mynpm",
			}),
			PYPI: bindings.r2({
				name: "stuff-mypypi",
			}),
			OCI: bindings.r2({
				name: "stuff-oci",
			}),
			MIRROR: bindings.r2({
				name: "stuff-mirror",
			}),
			OCI_BLOBS: bindings.r2({
				name: "stuff-oci-blobs",
			}),
			COORDINATOR: bindings.durableObject({ worker: "stuff", exportName: "PublicationCoordinator" }),
			STUFF_RELEASE: bindings.text("development"),
		},
	},
}));
