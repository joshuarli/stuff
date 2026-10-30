import { defineWranglerConfig } from "wrangler/experimental-config";

export default defineWranglerConfig({
	build: {
		command: "cargo install -q worker-build && worker-build --release",
	},
});
