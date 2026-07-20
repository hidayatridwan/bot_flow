# syntax=docker/dockerfile:1
#
# The web app had no deployment path at all until this file existed: the root Dockerfile builds only
# the two Rust binaries, and `bun run build` produced an artifact nothing ever ran.
#
# **Two runtimes on purpose.** bun installs and builds — it owns `bun.lock`, and it is what CI uses,
# so the dependency graph resolved here is the one that was tested. Node *runs* it, because
# `adapter-node` targets Node specifically and that is the combination its maintainers test. Using
# bun for both would be tidier and would put production on a path the adapter is not tested against.

### Stage 1: install + build
FROM oven/bun:1 AS build
WORKDIR /app

# Manifest first, so a source-only change reuses the install layer.
COPY package.json bun.lock ./
RUN --mount=type=cache,target=/root/.bun/install/cache \
    bun install --frozen-lockfile

COPY . .
RUN bun run build

### Stage 2: runtime
FROM node:22-slim AS web
WORKDIR /app
ENV NODE_ENV=production

# **Only `build/` is copied, and that is not an oversight.** `adapter-node` emits a self-contained
# bundle: verified by running `node build/index.js` in an empty directory with no `node_modules`
# present at all, which served `/login` and `/` correctly. So there is nothing to install here, and
# the image carries no package manager, no lockfile and no dependency tree to audit.
COPY --from=build /app/build ./build

# Drop root. The image ships in the `node` user's home, and nothing here writes to disk.
USER node

# adapter-node listens on 3000 unless PORT says otherwise. Note this is the *web* app's 3000, in its
# own container — the Rust API's 3000 is a different network namespace.
EXPOSE 3000

# Node's built-in fetch, because this image deliberately has no curl. `/login` rather than `/`:
# it exercises a real `load`, so a healthy answer means the server is actually rendering.
HEALTHCHECK --interval=10s --timeout=5s --start-period=10s --retries=5 \
    CMD node -e "fetch('http://127.0.0.1:'+(process.env.PORT||3000)+'/login').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["node", "build/index.js"]
