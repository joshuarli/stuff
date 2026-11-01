import { bindings, defineConfig, exports } from "cf/config";
import * as entrypoint from "./entry.ts" with { type: "cf-worker" };

export default defineConfig(({ mode }) => ({
	worker: {
		name: "stuff",
		compatibilityDate: "2026-10-30",
		entrypoint,
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
			"npm.17166969.xyz",
			"pypi.17166969.xyz",
			"stuff.17166969.xyz",
		],
		env: {
			STUFF_ROUTES: bindings.text("[{\"host\":\"crates.17166969.xyz\",\"bucket_binding\":\"MYCRATES\",\"key_prefix\":\"\"},{\"host\":\"npm.17166969.xyz\",\"bucket_binding\":\"MYNPM\",\"key_prefix\":\"\"},{\"host\":\"pypi.17166969.xyz\",\"bucket_binding\":\"PYPI\",\"key_prefix\":\"\"}]"),
			STUFF_UNIFIED_HOST: bindings.text("stuff.17166969.xyz"),
			MYCRATES: bindings.r2({
				name: "stuff-mycrates",
			}),
			MYNPM: bindings.r2({
				name: "stuff-mynpm",
			}),
			PYPI: bindings.r2({
				name: "stuff-mypypi",
			}),
		},
		// The Vite plugin treats an exports.default entrypoint as a named export
		// and emits `export const default`, so omit it while developing.
		...(mode === "development"
			? {}
			: {
					exports: {
						default: exports.worker({
							cache: {
								enabled: false,
							},
						}),
					},
				}),
	},
}));
