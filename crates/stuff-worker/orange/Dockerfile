FROM docker.io/library/node:26.13.1-bookworm-slim@sha256:5df79989ed685e09f0a98019db4cebfb22fb9df1a53c0161664a011f7ff6b0f1
RUN npm install --global --no-audit --no-fund pnpm@12.20.0
WORKDIR /orange
COPY orange/package.json orange/pnpm-lock.yaml orange/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile --prod --ignore-scripts && ln -s /orange/node_modules/open-orange/dist/bin.js /usr/local/bin/orange
WORKDIR /app
COPY app/ /app/
ENV NODE_ENV=production PORT=8080
EXPOSE 8080
USER node
CMD ["orange", "serve", "--config", "stuff.jsonc"]
