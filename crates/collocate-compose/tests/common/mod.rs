#![allow(dead_code)]
pub const FULL: &str = r#"
version: 1
project: myapp

nodes:
  edge-1:
    image: ubuntu:24.04
    cpus: 2
    memory: 2g
  edge-2:
    image: ubuntu:24.04
    cpus: 2
    memory: 2g
    target: host-a

services:
  db:
    node: edge-1
    series: "24.04"
    persistent: true
    cpus: 1
    memory: 1g
    volume: /var/lib/myapp/db-data:/var/lib/postgresql/data
    command: ["/usr/lib/postgresql/16/bin/postgres", "-D", "/var/lib/postgresql/data"]
    env:
      POSTGRES_USER: appuser
      POSTGRES_PASSWORD: ${secrets.db_password}
    secrets: [db_password]

  cache:
    node: edge-1
    image: redis:7
    publish: ["6379:6379"]

  server:
    node: edge-2
    series: "24.04"
    depends_on:
      - db
      - service: cache
        healthcheck: ["redis-cli", "ping"]
    publish: ["8080:8080"]
    command: ["/usr/bin/myserver", "--listen", "8080"]
    replicas: 3
    memory: 512m
    cpus: 1
    restart: on-failure:3
    tmpfs: ["/tmp:size=64m"]
    stop_signal: SIGINT
    stop_timeout: 30
    ulimits:
      nofile: 65536
    healthcheck:
      tcp: 8080
      interval: 5s
      timeout: 2s
      retries: 3
    autoscale:
      min: 2
      max: 10
      metrics:
        - type: cpu
          target: 65
    update:
      strategy: rolling
      max_surge: 1
      min_ready: 10s
    env:
      DATABASE_URL: "postgres://appuser:${secrets.db_password}@${services.db.address}:5432/appdb"
      REDIS_URL: "redis://${services.cache.address}:6379"
    secrets: [db_password]
    configs:
      app_settings: /etc/myapp/settings.yaml

secrets:
  db_password:
    generate: password
    length: 24
  registry_user:
    file: ./registry-user.txt

configs:
  app_settings:
    template: ./templates/settings.yaml.tmpl

registries:
  docker.io:
    username: ${secrets.registry_user}
    password: ${secrets.db_password}

loadbalancers:
  web:
    listen: 80
    publish: ["80:80"]
    backends:
      service: server
      port: 8080
    algorithm: round_robin
    health: from_service
    drain: 30s
"#;
