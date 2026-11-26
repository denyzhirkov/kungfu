# syntax=docker/dockerfile:1
ARG NODE_VERSION=20

FROM node:${NODE_VERSION}-alpine AS build
WORKDIR /app
ARG BUILD_ID
ENV NODE_ENV=production API_TOKEN=abc \
    LOG_LEVEL=info
COPY package.json ./
RUN npm ci && \
    npm run build
# comment between
EXPOSE 3000

FROM --platform=linux/amd64 nginx:1.25
env LEGACY_STYLE some value
COPY --from=build /app/dist /usr/share/nginx/html
USER nginx
cmd ["nginx", \
     "-g", "daemon off;"]
