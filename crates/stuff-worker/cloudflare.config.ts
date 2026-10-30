import { bindings, defineConfig } from "cf/config";

export default defineConfig({
	worker: {
		name: "stuff",
		compatibilityDate: "2026-10-30",
		entrypoint: "src/entry.ts",
		env: {
			STUFF_ROUTES: bindings.text("[{\"url_prefix\":\"mycrates\",\"bucket_binding\":\"MYCRATES\",\"key_prefix\":\"\"},{\"url_prefix\":\"mirror\",\"bucket_binding\":\"MYNPM\",\"key_prefix\":\"\"},{\"url_prefix\":\"pypi\",\"bucket_binding\":\"PYPI\",\"key_prefix\":\"\"}]"),
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
