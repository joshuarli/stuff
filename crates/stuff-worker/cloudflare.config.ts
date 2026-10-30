import { bindings, defineConfig } from "cf/config";

export default defineConfig({
	worker: {
		name: "stuff",
		compatibilityDate: "2026-10-30",
		entrypoint: "src/entry.ts",
		domains: [
			"crates.17166969.xyz",
			"npm.17166969.xyz",
			"pypi.17166969.xyz",
		],
		env: {
			STUFF_ROUTES: bindings.text("[{\"host\":\"crates.17166969.xyz\",\"bucket_binding\":\"MYCRATES\",\"key_prefix\":\"\"},{\"host\":\"npm.17166969.xyz\",\"bucket_binding\":\"MYNPM\",\"key_prefix\":\"\"},{\"host\":\"pypi.17166969.xyz\",\"bucket_binding\":\"PYPI\",\"key_prefix\":\"\"}]"),
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
	},
});
