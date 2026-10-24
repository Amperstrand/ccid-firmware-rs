/*
 * libifd-nucula.so — pcscd IFD handler v3 driver for the nucula wifi CCID
 * transport (issue #154/#157).
 *
 * Transport: TCP connect-out to the firmware's single-client server
 * (firmware/esp32-ccid/src/wifi_ccid.rs, default port 29443).
 *
 * Protocol: GemPC Twin serial framing, byte-for-byte the shared codec in
 * crates/ccid-transport-serial/src/lib.rs (the wifi profile uses the
 * default ServeConfig: silent errors, no unsolicited notify):
 *
 *   host->reader  [0x03 0x06 <10-byte CCID header> <payload, dwLength LE>
 *                  <LRC = XOR of all preceding bytes>]
 *   reader->host  ECHO of the received frame first, then the response
 *                  frame in the same shape
 *   NAK           [0x03 0x15 0x16]  (wifi profile never sends it)
 *   slot change   [0x50 xx]         (wifi profile never sends it)
 *
 * CCID header (both directions; message starts at frame offset 2):
 *   [0]    bMessageType
 *   [1..5] dwLength (LE)
 *   [5]    bSlot
 *   [6]    bSeq
 *   [7]    RDR: bStatus / PC: bSpecific_0 (bPowerSelect | bProtocolNum | bBWI)
 *   [8]    RDR: bError  / PC: bSpecific_1
 *   [9]    RDR: bChainParameter | bProtocolNum | bClockStatus / PC: bSpecific_2
 *
 * bStatus packing (ccid_protocol::status::build_bstatus): (cmd << 6) | icc
 * with icc in bits [1:0]: 0 = present+active, 1 = present+inactive,
 * 2 = absent.
 *
 * Lifecycle model: vsmartcard ifd-vpcd (a decade of production prior art)
 * — eject-on-error, reconnect-per-operation, presence = wire poll — plus
 * two hardenings vpcd lacks: SO_KEEPALIVE and SO_RCVTIMEO (see
 * docs/transport-research-154.md §5).
 *
 * Env knobs:
 *   NUCULA_POWER_OFF_SHIM=1   suppress IccPowerOff locally, answer success
 *                             (firmware #125 interim; mirrors
 *                             host-tools/nucula-ccid-broker.py)
 *   NUCULA_RECV_TIMEOUT_MS    per-read timeout, default 10000
 *   NUCULA_CONNECT_TIMEOUT_MS nonblocking-connect budget, default 1000
 *   NUCULA_LOG=1              syslog chatter (in addition to pcscd -d)
 *
 * License: GPL-2.0-or-later (same as the repo).
 */

#define _GNU_SOURCE 1 /* ppoll */

#include <ifdhandler.h>
#include <reader.h>

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <syslog.h>
#include <time.h>
#include <unistd.h>

#ifndef NUCULA_STANDALONE
#if __has_include(<debuglog.h>)
#include <debuglog.h>
#define NUC_HAVE_DEBUGLOG 1
#endif
#else
/* debuglog priorities without pcscd (standalone self-test binary) */
#define PCSC_LOG_DEBUG 0
#define PCSC_LOG_INFO 1
#define PCSC_LOG_ERROR 2
#define PCSC_LOG_CRITICAL 3
#endif

/* ---- wire constants (reference: crates/ccid-protocol/src/types.rs) ---- */

#define NUC_SYNC 0x03
#define NUC_CTRL_ACK 0x06
#define NUC_CTRL_NAK 0x15

#define NUC_PC_TO_RDR_ICC_POWER_ON 0x62
#define NUC_PC_TO_RDR_ICC_POWER_OFF 0x63
#define NUC_PC_TO_RDR_GET_SLOT_STATUS 0x65
#define NUC_PC_TO_RDR_XFR_BLOCK 0x6F
#define NUC_PC_TO_RDR_GET_PARAMETERS 0x6C
#define NUC_PC_TO_RDR_SET_PARAMETERS 0x61
#define NUC_PC_TO_RDR_ESCAPE 0x6B

#define NUC_RDR_TO_PC_DATA_BLOCK 0x80
#define NUC_RDR_TO_PC_SLOT_STATUS 0x81
#define NUC_RDR_TO_PC_PARAMETERS 0x82
#define NUC_RDR_TO_PC_ESCAPE 0x83

#define NUC_CCID_HEADER_SIZE 10
/* Reference-locked against the firmware codec (crates/ccid-protocol
 * types.rs MAX_CCID_MESSAGE_LENGTH=271; ccid-transport-serial lib.rs
 * MAX_CCID_PAYLOAD=261, MAX_FRAME_BYTES=274, header-stage reject of
 * payload_len > 261): a complete CCID bulk message (10-byte header +
 * payload) is capped at 271 bytes, so the maximum payload/dwLength is
 * 261 and the full wire frame is SYNC + CTRL + 271 + LRC = 274 bytes. */
#define NUC_MAX_CCID_MESSAGE 271                                  /* header + payload */
#define NUC_MAX_CCID_PAYLOAD (NUC_MAX_CCID_MESSAGE - NUC_CCID_HEADER_SIZE) /* 261 */
#define NUC_MAX_FRAME (2 + NUC_MAX_CCID_MESSAGE + 1)              /* 274 */

/* bmICCStatus values (ccid-protocol: PRESENT_ACTIVE=0, PRESENT_INACTIVE=1,
 * NO_ICC=2) */
#define NUC_ICC_ABSENT 0x02

#define NUC_DEFAULT_PORT 29443

/* Message field offsets inside a full frame (frame[0]=SYNC, frame[1]=CTRL) */
#define NUC_MSG_TYPE 2
#define NUC_MSG_LEN_LO 3
#define NUC_MSG_SEQ 8
#define NUC_MSG_STATUS 9
#define NUC_MSG_ERROR 10
#define NUC_MSG_PAYLOAD 12

/* ---- context ---- */

typedef struct {
    int fd;                 /* -1 = disconnected */
    char host[128];
    int port;
    unsigned char seq;
    unsigned char atr[MAX_ATR_SIZE];
    DWORD atr_len;
    int proto;              /* 0 = T=0, 1 = T=1 (last SetParameters) */
    pthread_mutex_t lock;
} nuc_ctx;

static nuc_ctx g_ctx = {
    .fd = -1,
    .port = NUC_DEFAULT_PORT,
    .seq = 0,
    .atr_len = 0,
    .proto = 1,
    .lock = PTHREAD_MUTEX_INITIALIZER,
};

static int g_power_off_shim = -1; /* lazy env read */

/* ---- logging ---- */

static void nuc_log(int priority, const char *fmt, ...)
{
    va_list ap;
    char buf[256];

    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
#ifdef NUC_HAVE_DEBUGLOG
    log_msg(priority, "%s", buf);
#endif
#ifdef NUCULA_STANDALONE
    fprintf(stderr, "[%d] %s\n", priority, buf);
#else
    if (getenv("NUCULA_LOG"))
        syslog(priority, "ifd-nucula: %s", buf);
#endif
}

#define NUC_LOG_ERR(...) nuc_log(PCSC_LOG_ERROR, __VA_ARGS__)
#define NUC_LOG_INFO(...) nuc_log(PCSC_LOG_INFO, __VA_ARGS__)

/* ---- socket layer ---- */

static void nuc_eject_locked(nuc_ctx *c)
{
    if (c->fd >= 0) {
        close(c->fd);
        c->fd = -1;
    }
}

static long nuc_env_ms(const char *name, long dflt)
{
    const char *v = getenv(name);
    if (!v || !*v)
        return dflt;
    return strtol(v, NULL, 0);
}

/* Nonblocking connect with a poll budget so a dead board can never stall
 * pcscd's reader thread (vpcd tolerates a down device; so do we). */
static int nuc_connect_locked(nuc_ctx *c)
{
    struct addrinfo hints, *res = NULL, *cur;
    char portstr[8];
    int fd = -1;
    long budget_ms = nuc_env_ms("NUCULA_CONNECT_TIMEOUT_MS", 1000);
    long rcv_ms = nuc_env_ms("NUCULA_RECV_TIMEOUT_MS", 10000);

    if (c->fd >= 0)
        return 0;

    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    snprintf(portstr, sizeof(portstr), "%d", c->port);
    if (getaddrinfo(c->host, portstr, &hints, &res) != 0 || !res) {
        NUC_LOG_ERR("resolve %s failed", c->host);
        return -1;
    }

    for (cur = res; cur; cur = cur->ai_next) {
        fd = socket(cur->ai_family, cur->ai_socktype, cur->ai_protocol);
        if (fd < 0)
            continue;
        int flags = fcntl(fd, F_GETFL, 0);
        if (flags >= 0)
            (void)fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        if (connect(fd, cur->ai_addr, cur->ai_addrlen) == 0)
            break;
        if (errno == EINPROGRESS) {
            struct pollfd pfd = { .fd = fd, .events = POLLOUT, .revents = 0 };
            struct timespec ts = { .tv_sec = budget_ms / 1000,
                                   .tv_nsec = (budget_ms % 1000) * 1000000 };
            if (ppoll(&pfd, 1, &ts, NULL) > 0) {
                int err = 0;
                socklen_t elen = sizeof(err);
                if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &elen) == 0
                    && err == 0)
                    break; /* connected */
            }
        }
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    if (fd < 0)
        return -1;

    {
        int flags = fcntl(fd, F_GETFL, 0);
        if (flags >= 0)
            (void)fcntl(fd, F_SETFL, flags & ~O_NONBLOCK);
    }

    int one = 1;
    (void)setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    /* Keepalive hardening (vpcd lacks this): a dead board is detected
     * within ~IDLE + INTVL*CNT even without write traffic. */
    (void)setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof(one));
    int ka = 5;
    (void)setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &ka, sizeof(ka));
    ka = 2;
    (void)setsockopt(fd, IPPROTO_TCP, TCP_KEEPINTVL, &ka, sizeof(ka));
    ka = 3;
    (void)setsockopt(fd, IPPROTO_TCP, TCP_KEEPCNT, &ka, sizeof(ka));

    struct timeval tv = { .tv_sec = rcv_ms / 1000,
                          .tv_usec = (suseconds_t)((rcv_ms % 1000) * 1000) };
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    c->fd = fd;
    return 0;
}

/* ---- frame codec (straight port of ccid-transport-serial semantics) ---- */

static unsigned char nuc_lrc(const unsigned char *buf, size_t len)
{
    unsigned char lrc = 0;
    for (size_t i = 0; i < len; i++)
        lrc ^= buf[i];
    return lrc;
}

/* read exactly n bytes; 0 ok, -1 error/timeout/EOF */
static int nuc_read_all_locked(nuc_ctx *c, unsigned char *buf, size_t n)
{
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(c->fd, buf + got, n - got);
        if (r > 0) {
            got += (size_t)r;
            continue;
        }
        if (r == 0)
            return -1; /* peer closed */
        if (r < 0 && errno == EINTR)
            continue;
        return -1; /* EAGAIN (rcvtimeo) or hard error */
    }
    return 0;
}

/*
 * Receive one full frame into frame[] in the SAME layout the builder
 * produces: [SYNC][CTRL][CCID message..][LRC]. Sync-anchored strict scan
 * (the repo's standing reader-test lesson: never assume a clean stream).
 * Returns frame length, 0 on NAK (protocol error), -1 on transport error.
 */
static int nuc_recv_frame_locked(nuc_ctx *c, unsigned char *frame)
{
    for (;;) {
        unsigned char b;
        if (nuc_read_all_locked(c, &b, 1) != 0)
            return -1;
        if (b != NUC_SYNC)
            continue; /* resync */
        if (nuc_read_all_locked(c, &b, 1) != 0)
            return -1;
        if (b == NUC_CTRL_NAK)
            return 0;
        if (b != NUC_CTRL_ACK)
            continue;
        frame[0] = NUC_SYNC;
        frame[1] = NUC_CTRL_ACK;
        break;
    }

    if (nuc_read_all_locked(c, frame + 2, NUC_CCID_HEADER_SIZE) != 0)
        return -1;
    uint32_t dwlen = (uint32_t)frame[NUC_MSG_LEN_LO]
        | ((uint32_t)frame[NUC_MSG_LEN_LO + 1] << 8)
        | ((uint32_t)frame[NUC_MSG_LEN_LO + 2] << 16)
        | ((uint32_t)frame[NUC_MSG_LEN_LO + 3] << 24);
    if (dwlen > NUC_MAX_CCID_PAYLOAD)
        return -1; /* cannot fit a legal frame — same header-stage
                    * reject as the firmware codec */
    if (dwlen > 0
        && nuc_read_all_locked(c, frame + NUC_MSG_PAYLOAD, dwlen) != 0)
        return -1;
    size_t body = NUC_MSG_PAYLOAD + dwlen; /* everything but the LRC */
    unsigned char lrc = 0;
    if (nuc_read_all_locked(c, &lrc, 1) != 0)
        return -1;
    if (nuc_lrc(frame, body) != lrc)
        return -1; /* desync — the wifi-profile firmware drops bad-LRC
                    * input silently, so seeing one means WE are desynced */
    frame[body] = lrc; /* store so echo verification sees the full frame */
    return (int)(body + 1);
}

static int nuc_send_frame_locked(nuc_ctx *c, const unsigned char *frame, size_t len)
{
    size_t sent = 0;
    while (sent < len) {
        /* MSG_NOSIGNAL: a library must never depend on the host
         * process's SIGPIPE disposition (pcscd's write to a dead
         * board must error, not kill the daemon) */
        ssize_t r = send(c->fd, frame + sent, len - sent, MSG_NOSIGNAL);
        if (r > 0) {
            sent += (size_t)r;
            continue;
        }
        if (r < 0 && errno == EINTR)
            continue;
        return -1;
    }
    return 0;
}

/* Build a PC_to_RDR frame; returns frame length, or 0 if the payload
 * cannot fit a legal frame (oversize payload — caller must treat 0 as
 * an error and must not send anything). */
static size_t nuc_build_request(unsigned char *frame, unsigned char type,
                                unsigned char seq, unsigned char spec0,
                                const unsigned char *payload, uint32_t plen)
{
    if (plen > NUC_MAX_CCID_PAYLOAD)
        return 0;
    frame[0] = NUC_SYNC;
    frame[1] = NUC_CTRL_ACK;
    frame[NUC_MSG_TYPE] = type;
    frame[NUC_MSG_LEN_LO] = (unsigned char)(plen & 0xFF);
    frame[NUC_MSG_LEN_LO + 1] = (unsigned char)((plen >> 8) & 0xFF);
    frame[NUC_MSG_LEN_LO + 2] = (unsigned char)((plen >> 16) & 0xFF);
    frame[NUC_MSG_LEN_LO + 3] = (unsigned char)((plen >> 24) & 0xFF);
    frame[7] = 0; /* bSlot */
    frame[NUC_MSG_SEQ] = seq;
    frame[NUC_MSG_STATUS] = spec0; /* PC->RDR specific-0 */
    frame[10] = 0;
    frame[11] = 0;
    if (plen && payload)
        memcpy(frame + NUC_MSG_PAYLOAD, payload, plen);
    size_t body = NUC_MSG_PAYLOAD + plen;
    frame[body] = nuc_lrc(frame, body);
    return body + 1;
}

/*
 * Typed transaction outcomes (Task 1.2 semantics — no blind retries):
 *
 *   NUC_TX_OK        response received, echo-verified
 *   NUC_TX_NOT_SENT  frame provably incomplete at the peer (send failed
 *                    mid-frame ⇒ at most a strict prefix delivered; a
 *                    GemPC frame is only complete with its last byte)
 *                    — the caller MAY safely retry
 *   NUC_TX_AMBIGUOUS frame fully handed to the transport and echo-
 *                    verified/underway, but the response never arrived:
 *                    the firmware may already have relayed the APDU to
 *                    the card. Resending could duplicate a state-
 *                    changing APDU (GP secure channel, eSIM, PIN) —
 *                    NEVER retry; surface the failure to pcscd.
 */
#define NUC_TX_OK 0
#define NUC_TX_NOT_SENT (-1)
#define NUC_TX_AMBIGUOUS (-2)

/*
 * One request/response exchange:
 *   connect (if needed) -> send -> read ECHO (verify byte-exact) ->
 *   read response.
 * Retry happens ONLY while the outcome is provably NUC_TX_NOT_SENT;
 * any post-send failure returns NUC_TX_AMBIGUOUS instead of resending.
 */
static int nuc_transact_locked(nuc_ctx *c, const unsigned char *req, size_t req_len,
                               unsigned char *resp, int *resp_len)
{
    unsigned char echo[NUC_MAX_FRAME];

    for (int attempt = 0; attempt < 2; attempt++) {
        if (nuc_connect_locked(c) != 0)
            return NUC_TX_NOT_SENT;

        if (nuc_send_frame_locked(c, req, req_len) != 0) {
            NUC_LOG_ERR("send failed (attempt %d) — not sent, retrying",
                        attempt + 1);
            nuc_eject_locked(c);
            continue;
        }

        int n = nuc_recv_frame_locked(c, echo);
        if (n <= 0 || (size_t)n != req_len || memcmp(echo, req, req_len) != 0) {
            NUC_LOG_ERR("echo mismatch/drop (n=%d) — frame was delivered, "
                        "outcome unknown, NOT retrying", n);
            nuc_eject_locked(c);
            return NUC_TX_AMBIGUOUS;
        }
        n = nuc_recv_frame_locked(c, resp);
        if (n <= 0) {
            NUC_LOG_ERR("no response — command may have executed, "
                        "NOT retrying");
            nuc_eject_locked(c);
            return NUC_TX_AMBIGUOUS;
        }
        *resp_len = n;
        return NUC_TX_OK;
    }
    return NUC_TX_NOT_SENT;
}

/* Convenience: build + transact + bSeq echo check (CCID 6.2-1; the
 * firmware regression-locks it). 0 ok / -1 transport. */
static int nuc_resp_cmd_failed(const unsigned char *resp);

static int nuc_command(unsigned char type, unsigned char spec0,
                       const unsigned char *payload, uint32_t plen,
                       unsigned char *resp, int *resp_len)
{
    unsigned char req[NUC_MAX_FRAME];
    unsigned char seq = g_ctx.seq++;
    size_t req_len = nuc_build_request(req, type, seq, spec0, payload, plen);
    if (req_len == 0 || req_len > NUC_MAX_FRAME) {
        NUC_LOG_ERR("payload %u exceeds frame budget (%d)",
                    (unsigned)plen, (int)NUC_MAX_CCID_PAYLOAD);
        return -1;
    }
    int rc = nuc_transact_locked(&g_ctx, req, req_len, resp, resp_len);

    /* CCID 6.2-2 time extension: a SlotStatus with bmCommandStatus=0
     * ("still processing") and OUR bSeq means the reader bought more
     * time — it is not the final answer. Only command types that carry
     * card work get extensions (GetSlotStatus/GetParameters never do,
     * and they legitimately answer with a SlotStatus as their FINAL
     * frame, so they must be excluded). Keep reading until a final
     * frame, a failure status, or the receive timeout. */
    int may_extend = (type == NUC_PC_TO_RDR_XFR_BLOCK
                      || type == NUC_PC_TO_RDR_ICC_POWER_ON
                      || type == NUC_PC_TO_RDR_ESCAPE);
    for (int ext = 0;
         rc == 0 && may_extend && ext < 64
         && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_SLOT_STATUS
         && !nuc_resp_cmd_failed(resp)
         && resp[NUC_MSG_SEQ] == seq;
         ext++) {
        int n2 = nuc_recv_frame_locked(&g_ctx, resp);
        if (n2 <= 0) {
            NUC_LOG_ERR("transport died waiting out a time extension");
            nuc_eject_locked(&g_ctx);
            return -1;
        }
        *resp_len = n2;
    }

    if (rc == 0 && resp[NUC_MSG_SEQ] != seq) {
        NUC_LOG_ERR("bSeq mismatch: sent %u got %u", seq, resp[NUC_MSG_SEQ]);
        /* the stream is desynced (stale frame): drop the connection so
         * the next command starts clean instead of chasing ghosts */
        nuc_eject_locked(&g_ctx);
        return -1;
    }
    return rc;
}

/* ---- response helpers (message fields at NUC_MSG_* offsets) ---- */

static int nuc_resp_cmd_failed(const unsigned char *resp)
{
    return (resp[NUC_MSG_STATUS] & 0xC0) != 0; /* bmCommandStatus [7:6] */
}

static int nuc_resp_icc_absent(const unsigned char *resp)
{
    return (resp[NUC_MSG_STATUS] & 0x03) == NUC_ICC_ABSENT;
}

static uint32_t nuc_resp_payload_len(const unsigned char *resp)
{
    return (uint32_t)resp[NUC_MSG_LEN_LO]
        | ((uint32_t)resp[NUC_MSG_LEN_LO + 1] << 8)
        | ((uint32_t)resp[NUC_MSG_LEN_LO + 2] << 16)
        | ((uint32_t)resp[NUC_MSG_LEN_LO + 3] << 24);
}

/* Cache the ATR (also served by IFDHGetCapabilities TAG_IFD_ATR). */
static void nuc_cache_atr(const unsigned char *resp)
{
    uint32_t n = nuc_resp_payload_len(resp);
    if (n > MAX_ATR_SIZE)
        n = MAX_ATR_SIZE;
    if (n)
        memcpy(g_ctx.atr, resp + NUC_MSG_PAYLOAD, n);
    g_ctx.atr_len = n;
}

/*
 * Walk the ATR for the T=1 interface bytes. ATR semantics (ISO 7816-3 /
 * CCID §6.1.7 note): TDi's high nibble = Y(i+1) flags for the NEXT group's
 * interface bytes; low nibble = protocol those bytes belong to. When the
 * pending protocol is T=1, the next group's TA = IFSC and TB = BWI<<4|CWI.
 * Bench ATR example (G-05): 3B 85 81 31 FE 45 .. -> TD1=0x81 (proto 1,
 * TD2 follows), TD2=0x31 (TA3+TB3 follow, proto 1) -> IFSC=0xFE,
 * BWI/CWI=0x45.
 */
static void nuc_t1_from_atr(const unsigned char *atr, DWORD len,
                            unsigned char *ifsc, unsigned char *wi)
{
    unsigned char next_y, pending_proto = 0;
    DWORD pos;

    *ifsc = 0xFE; /* G-05 default (matches the firmware ATR builder) */
    *wi = 0x45;   /* BWI=4, CWI=5 */

    if (len < 2 || atr[0] != 0x3B)
        return;
    next_y = atr[1] >> 4;
    pos = 2;
    while (pos < len) {
        unsigned char y = next_y;
        unsigned char td = 0;
        int have_td = 0;

        /* every interface byte is bounds-checked: a truncated ATR that
         * still promises more groups stops at the first missing byte
         * (defaults above stay in force) instead of reading past it */
        if (y & 0x01) { /* TA(i) */
            if (pos >= len)
                break;
            if (pending_proto == 1 && atr[pos])
                *ifsc = atr[pos];
            pos++;
        }
        if (y & 0x02) { /* TB(i) */
            if (pos >= len)
                break;
            if (pending_proto == 1)
                *wi = atr[pos];
            pos++;
        }
        if (y & 0x04) { /* TC(i) */
            if (pos >= len)
                break;
            pos++;
        }
        if (y & 0x08) { /* TD(i) */
            if (pos >= len)
                break;
            td = atr[pos];
            pos++;
            have_td = 1;
        }
        if (!have_td)
            break;
        next_y = td >> 4;
        pending_proto = td & 0x0F;
    }
}

/* ---- ifdhandler v3 entry points ---- */

static RESPONSECODE nuc_parse_devicename(const char *devname)
{
    /* "host:port" (our own parser — pcscd hands DEVICENAME through
     * verbatim; the libccidtwin first-colon split is not our problem).
     * Bare host keeps the configured port. */
    char buf[sizeof(g_ctx.host) + 16];

    if (!devname || !*devname)
        return IFD_COMMUNICATION_ERROR;
    snprintf(buf, sizeof(buf), "%s", devname);
    char *colon = strchr(buf, ':');
    if (colon) {
        long p = strtol(colon + 1, NULL, 0);
        if (p > 0 && p <= 65535)
            g_ctx.port = (int)p;
        *colon = '\0';
    }
    size_t hl = strlen(buf);
    if (hl == 0 || hl >= sizeof(g_ctx.host))
        return IFD_COMMUNICATION_ERROR;
    memcpy(g_ctx.host, buf, hl + 1);
    return IFD_SUCCESS;
}

static RESPONSECODE nuc_create(const char *devname)
{
    if (nuc_parse_devicename(devname) != IFD_SUCCESS)
        return IFD_SUCCESS; /* bad name: reader still loads, card absent */

    /* Try to connect + identity-probe; failure is NOT fatal (vpcd
     * precedent — reader stays listed, presence poll re-connects). */
    pthread_mutex_lock(&g_ctx.lock);
    if (nuc_connect_locked(&g_ctx) == 0) {
        unsigned char resp[NUC_MAX_FRAME];
        int rn = 0;
        if (nuc_command(NUC_PC_TO_RDR_ESCAPE, 0, (const unsigned char *)"\x02", 1,
                        resp, &rn) == 0
            && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_ESCAPE
            && !nuc_resp_cmd_failed(resp)) {
            NUC_LOG_INFO("connected to %s:%d (id ok)", g_ctx.host, g_ctx.port);
        }
    } else {
        NUC_LOG_INFO("board %s:%d down at create — deferred to presence poll",
                     g_ctx.host, g_ctx.port);
    }
    pthread_mutex_unlock(&g_ctx.lock);
    return IFD_SUCCESS;
}

RESPONSECODE IFDHCreateChannelByName(DWORD Lun, LPSTR DeviceName)
{
    (void)Lun;
    return nuc_create(DeviceName);
}

RESPONSECODE IFDHCreateChannel(DWORD Lun, DWORD Channel)
{
    /* vpcd convention: CHANNELID is the port; host from NUCULA_HOST.
     * Channels below 1024 are treated as "not a port" so a stray
     * CHANNELID 1 does not send us to tcp/1. */
    const char *host = getenv("NUCULA_HOST");
    char dev[192];
    unsigned long port = (Channel >= 1024 && Channel <= 65535)
        ? (unsigned long)Channel
        : (unsigned long)NUC_DEFAULT_PORT;

    (void)Lun;
    snprintf(dev, sizeof(dev), "%s:%lu", (host && *host) ? host : "127.0.0.1", port);
    return nuc_create(dev);
}

RESPONSECODE IFDHCloseChannel(DWORD Lun)
{
    (void)Lun;
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);
    return IFD_SUCCESS;
}

RESPONSECODE IFDHGetCapabilities(DWORD Lun, DWORD Tag, PDWORD Length, PUCHAR Value)
{
    (void)Lun;
    if (!Length || !Value)
        return IFD_COMMUNICATION_ERROR;

    switch (Tag) {
    case TAG_IFD_ATR:
    case SCARD_ATTR_ATR_STRING:
        if (*Length < g_ctx.atr_len) {
            *Length = g_ctx.atr_len;
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        }
        if (g_ctx.atr_len)
            memcpy(Value, g_ctx.atr, g_ctx.atr_len);
        *Length = g_ctx.atr_len;
        return IFD_SUCCESS;
    case TAG_IFD_SLOTS_NUMBER:
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 1;
        *Length = 1;
        return IFD_SUCCESS;
    case TAG_IFD_THREAD_SAFE:
        /* single global ctx + mutex; pcscd serializes (vpcd precedent) */
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 0;
        *Length = 1;
        return IFD_SUCCESS;
    default:
        *Length = 0;
        return IFD_ERROR_TAG;
    }
}

RESPONSECODE IFDHSetCapabilities(DWORD Lun, DWORD Tag, DWORD Length, PUCHAR Value)
{
    (void)Lun;
    (void)Tag;
    (void)Length;
    (void)Value;
    return IFD_NOT_SUPPORTED;
}

RESPONSECODE IFDHSetProtocolParameters(DWORD Lun, DWORD Protocol, UCHAR Flags,
                                       UCHAR PTS1, UCHAR PTS2, UCHAR PTS3)
{
    (void)Lun;
    (void)Flags;
    (void)PTS2;
    (void)PTS3;

    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;
    int is_t1 = (Protocol == SCARD_PROTOCOL_T1);
    RESPONSECODE r = IFD_COMMUNICATION_ERROR;

    pthread_mutex_lock(&g_ctx.lock);

    unsigned char params[7];
    uint32_t plen;
    if (is_t1) {
        unsigned char ifsc, wi;
        nuc_t1_from_atr(g_ctx.atr, g_ctx.atr_len, &ifsc, &wi);
        /* CCID 6.1.7 T=1 abProtocolDataStructure (7 bytes) — mirrored
         * against the firmware parser (ccid-core handler.rs
         * handle_set_parameters: IFSC 0x10..0xFE, NAD shall be 0). */
        params[0] = PTS1 ? PTS1 : 0x11; /* Fi/Di */
        params[1] = 0x00;               /* bmTCCKST1: LRC, direct conv */
        params[2] = 0x00;               /* bGuardTime */
        params[3] = wi;                 /* bmWaitingIntegers */
        params[4] = 0x00;               /* bClockStop */
        params[5] = ifsc;               /* bIFSC */
        params[6] = 0x00;               /* bNadValue */
        plen = 7;
    } else {
        /* T=0: 5 bytes [FiDi][bmTCCKST0][bGuardTime][bClockStop]
         * [bWaitingInteger] */
        params[0] = PTS1 ? PTS1 : 0x11;
        params[1] = 0x00;
        params[2] = 0x00;
        params[3] = 0x00;
        params[4] = 0x0A;
        plen = 5;
    }

    /* Forward it — receiving SetParameters(T=1) ARMS the firmware's T=1
     * endpoint (ccid_handler.rs t1_armed). Never no-op this (vpcd can
     * afford to; we cannot). */
    if (nuc_command(NUC_PC_TO_RDR_SET_PARAMETERS, (unsigned char)(is_t1 ? 1 : 0),
                    params, plen, resp, &rn) == 0) {
        if (nuc_resp_cmd_failed(resp)) {
            NUC_LOG_ERR("SetParameters rejected (bError=0x%02x)",
                        resp[NUC_MSG_ERROR]);
        } else {
            g_ctx.proto = is_t1 ? 1 : 0;
            r = IFD_SUCCESS;
        }
    }
    pthread_mutex_unlock(&g_ctx.lock);
    return r;
}

RESPONSECODE IFDHPowerICC(DWORD Lun, DWORD Action, PUCHAR Atr, PDWORD AtrLength)
{
    (void)Lun;
    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;
    RESPONSECODE r = IFD_COMMUNICATION_ERROR;

    if (!AtrLength)
        return IFD_COMMUNICATION_ERROR;
    DWORD orig = *AtrLength;
    *AtrLength = 0;

    if (g_power_off_shim < 0) {
        const char *v = getenv("NUCULA_POWER_OFF_SHIM");
        g_power_off_shim = (v && v[0] == '1');
    }

    pthread_mutex_lock(&g_ctx.lock);

    switch (Action) {
    case IFD_POWER_DOWN:
        if (g_power_off_shim) {
            /* #125 interim: answer locally (mirrors the CDC broker's
             * PowerOffShim) so pcscd's open-sequence power-off cannot
             * trip the firmware re-arm gap. */
            NUC_LOG_INFO("power-off answered locally (#125 interim)");
            r = IFD_SUCCESS;
        } else if (nuc_command(NUC_PC_TO_RDR_ICC_POWER_OFF, 0, NULL, 0, resp,
                               &rn) == 0
                   && !nuc_resp_cmd_failed(resp)) {
            r = IFD_SUCCESS;
        }
        break;

    case IFD_POWER_UP:
    case IFD_RESET:
        /* The firmware re-activates on IccPowerOn (lazy-teardown model
         * rebuilds from the cached auto-ATS; fresh start selects). */
        if (nuc_command(NUC_PC_TO_RDR_ICC_POWER_ON, 0x00 /* auto */, NULL, 0, resp,
                        &rn) == 0) {
            if (resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_DATA_BLOCK
                && !nuc_resp_cmd_failed(resp)) {
                nuc_cache_atr(resp); /* cache first: TAG_IFD_ATR keeps
                                      * serving the ATR even when the
                                      * caller buffer cannot take it */
                /* #457: a caller buffer smaller than the ATR is
                 * IFD_ERROR_INSUFFICIENT_BUFFER with the required size
                 * in *AtrLength (same contract as IFDHGetCapabilities),
                 * never a silent SUCCESS that drops the ATR. */
                DWORD avail = Atr ? orig : 0;
                if (g_ctx.atr_len > avail) {
                    *AtrLength = g_ctx.atr_len;
                    r = IFD_ERROR_INSUFFICIENT_BUFFER;
                } else {
                    if (Atr && g_ctx.atr_len)
                        memcpy(Atr, g_ctx.atr, g_ctx.atr_len);
                    *AtrLength = g_ctx.atr_len;
                    r = IFD_SUCCESS;
                }
            } else {
                NUC_LOG_ERR("power-on failed (bError=0x%02x)",
                            resp[NUC_MSG_ERROR]);
            }
        }
        break;

    default:
        r = IFD_NOT_SUPPORTED;
        break;
    }

    pthread_mutex_unlock(&g_ctx.lock);
    return r;
}

RESPONSECODE IFDHTransmitToICC(DWORD Lun, SCARD_IO_HEADER SendPci, PUCHAR TxBuffer,
                               DWORD TxLength, PUCHAR RxBuffer, PDWORD RxLength,
                               PSCARD_IO_HEADER RecvPci)
{
    (void)Lun;
    (void)SendPci;
    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;

    if (!RxLength || !RecvPci)
        return IFD_COMMUNICATION_ERROR;
    if (TxLength > NUC_MAX_CCID_PAYLOAD) {
        /* a legal frame cannot carry this payload; refuse before any
         * buffer is touched (261 = short-APDU TPDU max) */
        *RxLength = 0;
        return IFD_COMMUNICATION_ERROR;
    }
    DWORD orig = *RxLength;
    *RxLength = 0;

    pthread_mutex_lock(&g_ctx.lock);
    int rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, TxBuffer, TxLength, resp, &rn);
    RESPONSECODE r = IFD_COMMUNICATION_ERROR;
    if (rc == 0) {
        if (resp[NUC_MSG_TYPE] != NUC_RDR_TO_PC_DATA_BLOCK) {
            NUC_LOG_ERR("XfrBlock answered type 0x%02x (expected DataBlock)",
                        resp[NUC_MSG_TYPE]);
        } else if (nuc_resp_cmd_failed(resp) && nuc_resp_icc_absent(resp)) {
            r = IFD_ICC_NOT_PRESENT;
        } else if (nuc_resp_cmd_failed(resp)) {
            /* failed XfrBlock with the card still seated is a CCID
             * command failure (bError), never a successful APDU
             * exchange with a phantom payload */
            NUC_LOG_ERR("XfrBlock failed (bError=0x%02x)",
                        resp[NUC_MSG_ERROR]);
        } else {
            uint32_t n = nuc_resp_payload_len(resp);
            if (n > orig) {
                NUC_LOG_ERR("response %u > buffer %lu", n, (unsigned long)orig);
            } else {
                if (n)
                    memcpy(RxBuffer, resp + NUC_MSG_PAYLOAD, n);
                *RxLength = n;
                RecvPci->Protocol =
                    g_ctx.proto ? SCARD_PROTOCOL_T1 : SCARD_PROTOCOL_T0;
                r = IFD_SUCCESS;
            }
        }
    }
    pthread_mutex_unlock(&g_ctx.lock);
    return r;
}

RESPONSECODE IFDHICCPresence(DWORD Lun)
{
    (void)Lun;
    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;

    pthread_mutex_lock(&g_ctx.lock);
    /* The reconnect engine: every poll (pcscd cadence ~400 ms) re-connects
     * a down board and re-evaluates presence from the wire. */
    int rc = nuc_command(NUC_PC_TO_RDR_GET_SLOT_STATUS, 0, NULL, 0, resp, &rn);
    RESPONSECODE r;
    if (rc != 0) {
        /* vpcd precedent: comm failure maps to card-absent — the reader
         * stays listed in pcscd and recovers when the board returns. */
        r = IFD_ICC_NOT_PRESENT;
    } else {
        r = nuc_resp_icc_absent(resp) ? IFD_ICC_NOT_PRESENT : IFD_ICC_PRESENT;
    }
    pthread_mutex_unlock(&g_ctx.lock);
    return r;
}

RESPONSECODE IFDHControl(DWORD Lun, DWORD ControlCode, PUCHAR TxBuffer, DWORD TxLength,
                         PUCHAR RxBuffer, DWORD RxLength, LPDWORD pdwBytesReturned)
{
    (void)Lun;

    if (!pdwBytesReturned)
        return IFD_COMMUNICATION_ERROR;
    *pdwBytesReturned = 0;

    if (ControlCode == CM_IOCTL_GET_FEATURE_REQUEST) {
        /* exactly one feature: CCID escape passthrough (0xD0 counters,
         * 0xD2/0xD3 bench probes become SCardControl calls). TLV value
         * is BIG-endian — libccid assigns htonl(ioctl) into
         * PCSC_TLV_STRUCTURE.value (ccid_ifdhandler.c), clients parse
         * it big-endian; smoke-verified against pcscd 2.0.3 (a
         * little-endian write reached pcscd as 0x13000042). */
        if (RxLength < 6)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        uint32_t ioctl = (uint32_t)SCARD_CTL_CODE(FEATURE_CCID_ESC_COMMAND);
        RxBuffer[0] = (unsigned char)FEATURE_CCID_ESC_COMMAND;
        RxBuffer[1] = 4;
        RxBuffer[2] = (unsigned char)((ioctl >> 24) & 0xFF);
        RxBuffer[3] = (unsigned char)((ioctl >> 16) & 0xFF);
        RxBuffer[4] = (unsigned char)((ioctl >> 8) & 0xFF);
        RxBuffer[5] = (unsigned char)(ioctl & 0xFF);
        *pdwBytesReturned = 6;
        return IFD_SUCCESS;
    }

    if (ControlCode == (DWORD)SCARD_CTL_CODE(FEATURE_CCID_ESC_COMMAND)) {
        unsigned char resp[NUC_MAX_FRAME];
        int rn = 0;
        if (TxLength > NUC_MAX_CCID_PAYLOAD)
            return IFD_COMMUNICATION_ERROR;
        pthread_mutex_lock(&g_ctx.lock);
        int rc = nuc_command(NUC_PC_TO_RDR_ESCAPE, 0, TxBuffer, TxLength, resp, &rn);
        RESPONSECODE r = IFD_COMMUNICATION_ERROR;
        if (rc == 0) {
            if (nuc_resp_cmd_failed(resp)) {
                NUC_LOG_ERR("escape failed (bError=0x%02x)",
                            resp[NUC_MSG_ERROR]);
            } else {
                uint32_t n = nuc_resp_payload_len(resp);
                if (n > RxLength)
                    n = RxLength;
                if (n)
                    memcpy(RxBuffer, resp + NUC_MSG_PAYLOAD, n);
                *pdwBytesReturned = n;
                r = IFD_SUCCESS;
            }
        }
        pthread_mutex_unlock(&g_ctx.lock);
        return r;
    }

    return IFD_ERROR_NOT_SUPPORTED;
}

/* ---- standalone self-test (make selftest; mock board on the other end) ---- */

#ifdef NUCULA_STANDALONE

static int st_failures = 0;

static void st_check(int cond, const char *what)
{
    printf("%-52s %s\n", what, cond ? "PASS" : "FAIL");
    if (!cond)
        st_failures++;
}

static void st_hex(const char *label, const unsigned char *b, size_t n)
{
    printf("%-52s ", label);
    for (size_t i = 0; i < n; i++)
        printf("%02X ", b[i]);
    printf("\n");
}

int main(int argc, char **argv)
{
    const char *host = argc > 1 ? argv[1] : "127.0.0.1";
    int port = argc > 2 ? atoi(argv[2]) : NUC_DEFAULT_PORT;
    char dev[192];
    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;
    int ok;

    snprintf(dev, sizeof(dev), "%s:%d", host, port);
    printf("== ifd-nucula standalone self-test against %s ==\n", dev);
    if (IFDHCreateChannelByName(0, dev) != IFD_SUCCESS) {
        printf("create channel failed\n");
        return 2;
    }

    /* identity */
    ok = nuc_command(NUC_PC_TO_RDR_ESCAPE, 0, (const unsigned char *)"\x02", 1,
                     resp, &rn) == 0
        && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_ESCAPE && !nuc_resp_cmd_failed(resp);
    st_check(ok, "escape 0x02 identity answered");

    /* slot status */
    ok = nuc_command(NUC_PC_TO_RDR_GET_SLOT_STATUS, 0, NULL, 0, resp, &rn) == 0
        && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_SLOT_STATUS;
    st_check(ok, "GetSlotStatus answered");
    if (ok)
        printf("%-52s bStatus=0x%02x (%s)\n", "slot status", resp[NUC_MSG_STATUS],
               nuc_resp_icc_absent(resp) ? "absent" : "present");

    /* power on -> ATR */
    ok = nuc_command(NUC_PC_TO_RDR_ICC_POWER_ON, 0, NULL, 0, resp, &rn) == 0
        && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_DATA_BLOCK
        && !nuc_resp_cmd_failed(resp);
    st_check(ok, "IccPowerOn DataBlock");
    if (ok) {
        nuc_cache_atr(resp);
        st_hex("ATR:", g_ctx.atr, g_ctx.atr_len);
    }

    /* set parameters T=1 (arms the firmware endpoint) */
    {
        unsigned char p[7] = { 0x11, 0x00, 0x00, 0x45, 0x00, 0xFE, 0x00 };
        ok = nuc_command(NUC_PC_TO_RDR_SET_PARAMETERS, 1, p, 7, resp, &rn) == 0
            && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_PARAMETERS
            && !nuc_resp_cmd_failed(resp);
        st_check(ok, "SetParameters(T=1) accepted");
    }

    /* transmit: SELECT by AID (zero-risk set, bench doctrine) */
    {
        static const unsigned char sel[] = { 0x00, 0xA4, 0x04, 0x00 };
        ok = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, sel, sizeof(sel), resp, &rn) == 0
            && resp[NUC_MSG_TYPE] == NUC_RDR_TO_PC_DATA_BLOCK;
        st_check(ok, "XfrBlock SELECT answered");
        if (ok)
            st_hex("R-APDU:", resp + NUC_MSG_PAYLOAD, nuc_resp_payload_len(resp));
    }

    /* presence through the ifdhandler entry point */
    st_check(IFDHICCPresence(0) == IFD_ICC_PRESENT, "IFDHICCPresence == PRESENT");

    /* power off */
    ok = nuc_command(NUC_PC_TO_RDR_ICC_POWER_OFF, 0, NULL, 0, resp, &rn) == 0
        && !nuc_resp_cmd_failed(resp);
    st_check(ok, "IccPowerOff answered");

    IFDHCloseChannel(0);
    printf("== %s (%d failures) ==\n", st_failures ? "FAIL" : "ALL PASS", st_failures);
    return st_failures ? 1 : 0;
}

#endif /* NUCULA_STANDALONE */
