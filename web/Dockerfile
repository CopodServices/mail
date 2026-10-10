# syntax=docker/dockerfile:1

# cPOD Mail web client: a static, client-rendered bundle served by nginx, which
# also reverse-proxies /api/* to the mail API. There is no Node server.

FROM oven/bun:1 AS deps
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile

FROM oven/bun:1 AS build
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
# Baked into the bundle. Leave empty to call the API through this origin (/api).
ARG NEXT_PUBLIC_MAIL_API_URL=""
ARG NEXT_PUBLIC_SOURCE_URL="https://github.com/copodservices/mail"
ENV NEXT_PUBLIC_MAIL_API_URL=$NEXT_PUBLIC_MAIL_API_URL
ENV NEXT_PUBLIC_SOURCE_URL=$NEXT_PUBLIC_SOURCE_URL
RUN bun run build

FROM nginx:1.27-alpine AS runtime
COPY --from=build /app/out /usr/share/nginx/html
# The image entrypoint substitutes this into the config template when the
# container starts, so the API can move without rebuilding the bundle. Keep it
# a bare origin: the /api prefix is forwarded unchanged.
ENV MAIL_API_URL=http://mail-api:8080
# Substitute only this variable, so nginx directives such as $uri and
# $http_upgrade in the template are left alone.
ENV NGINX_ENVSUBST_FILTER="^MAIL_API_URL$"
COPY nginx/default.conf.template /etc/nginx/templates/default.conf.template
# The stock config serves :80; this image serves :8080 only.
RUN rm -f /etc/nginx/conf.d/default.conf
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s CMD wget -q -O /dev/null http://127.0.0.1:8080/ || exit 1
