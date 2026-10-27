FROM node:22-bookworm-slim AS build
WORKDIR /app
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml tsconfig.json ./
COPY patches ./patches
COPY packages ./packages
COPY plugins ./plugins
COPY apps/web ./apps/web
RUN pnpm install --frozen-lockfile --filter @anlg/web...
ARG VITE_APP_URL
ARG VITE_API_URL
ARG VITE_SUPABASE_URL
ARG VITE_SUPABASE_ANON_KEY
ENV VITE_APP_URL=$VITE_APP_URL \
    VITE_API_URL=$VITE_API_URL \
    VITE_SUPABASE_URL=$VITE_SUPABASE_URL \
    VITE_SUPABASE_ANON_KEY=$VITE_SUPABASE_ANON_KEY \
    SUPABASE_URL=$VITE_SUPABASE_URL \
    SUPABASE_ANON_KEY=$VITE_SUPABASE_ANON_KEY \
    ANARLOG_SHARED_RECORDINGS_PREVIEW=1 \
    NITRO_PRESET=node-server
RUN pnpm -F @anlg/ui build && pnpm -F @anlg/web build

FROM node:22-bookworm-slim AS runtime
ENV NODE_ENV=production HOST=0.0.0.0 PORT=3000 ANARLOG_SHARED_RECORDINGS_PREVIEW=1
WORKDIR /app
COPY --from=build --chown=node:node /app/apps/web/.output ./
USER node
CMD ["node", "server/index.mjs"]
