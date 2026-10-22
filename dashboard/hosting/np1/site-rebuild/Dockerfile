# Sidecar that runs rebuild.sh hourly. Needs the Docker and Uncloud sockets, /root/ployz, and the ployz-site deploy key
# mounted from np1.
FROM docker:28-cli
RUN apk add --no-cache git openssh-client
ARG SUPERCRONIC_VERSION=v0.2.51
ARG SUPERCRONIC_SHA256
RUN wget -q -O /usr/local/bin/supercronic "https://github.com/aptible/supercronic/releases/download/${SUPERCRONIC_VERSION}/supercronic-linux-amd64" \
  && echo "${SUPERCRONIC_SHA256}  /usr/local/bin/supercronic" | sha256sum -c - \
  && chmod +x /usr/local/bin/supercronic
COPY uc /usr/local/bin/uc
COPY rebuild.sh /usr/local/bin/rebuild.sh
COPY crontab /etc/crontab
ENTRYPOINT ["supercronic", "/etc/crontab"]
