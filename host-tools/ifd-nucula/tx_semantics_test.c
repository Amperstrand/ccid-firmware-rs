/*
 * tx_semantics_test.c — fault-injection regression for the nuc_transact
 * retry policy (hardening WP H-003, Task 1.2): after a frame has been
 * fully handed to the transport, no failure may cause the driver to
 * resend it — a duplicated XfrBlock can duplicate a state-changing
 * APDU (GP secure channel, eSIM, PIN).
 *
 * A scripted fake board (own thread) accepts connections and applies
 * one fault step per connection, mirroring the firmware's GemPC
 * semantics (full-frame read -> echo -> response):
 *
 *   STEP_NONE          echo + answer (baseline)
 *   STEP_REFUSE        listener closed: connect cannot succeed
 *   STEP_CLOSE_EARLY   close immediately (loss before/during send)
 *   STEP_MIDFRAME      read 5 bytes, close (loss during send)
 *   STEP_NO_ECHO       read the FULL frame, close without echoing
 *                      (loss after send; card may have executed)
 *   STEP_NO_RESPONSE   echo byte-exact, close without answering
 *                      (loss after card execution)
 *
 * Invariants asserted:
 *   - NO_ECHO / NO_RESPONSE (frame fully readable by the board):
 *     exactly ONE complete frame ever reaches it (pre-fix: two — the
 *     duplicate) and the driver reports AMBIGUOUS, never success
 *   - MIDFRAME / CLOSE_EARLY (peer dies mid-read): a small-frame
 *     write() always completes locally into the kernel, so peer-side
 *     death is locally indistinguishable from post-execution death —
 *     the driver must classify AMBIGUOUS (no resend), not retry
 *   - true send failure (dead-write socketpair injected into the
 *     ctx): retry IS allowed and recovers (TX_OK) — send failure
 *     means at most a strict frame prefix was delivered, which can
 *     never execute
 *
 * Build/run: make tx-semantics   (ASan+UBSan)
 *
 * License: GPL-2.0-or-later (same as the repo).
 */

#include "ifd-nucula.c"

#include <arpa/inet.h>
#include <pthread.h>
#include <signal.h>
#include <sys/socket.h>

void log_msg(const int priority, const char *fmt, ...)
{
    va_list ap;
    fprintf(stderr, "[pcsc:%d] ", priority);
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}

/* ---- fake board ---- */

enum {
    STEP_NONE = 0,
    STEP_REFUSE,
    STEP_CLOSE_EARLY,
    STEP_MIDFRAME,
    STEP_NO_ECHO,
    STEP_NO_RESPONSE,
    /* response-decode steps (Task 1.4): the board behaves normally on
     * the wire but answers with hostile/malformed CCID responses */
    STEP_BAD_SEQ,
    STEP_CMD_FAILED_PRESENT,
    STEP_TIME_EXTENSION,
    STEP_TIME_EXTENSION_DATABLOCK,
    STEP_TIME_EXTENSION_CHAIN,
    STEP_WRONG_TYPE
};

struct fake_board {
    int listen_fd;
    int steps[8];
    int nsteps;
    pthread_mutex_t lock;
    pthread_cond_t cv;
    int conns_done;      /* connections fully handled AND counted */
    int serving;         /* a connection is accepted but not yet counted */
    int frames_complete; /* full frames received (= would-be executions) */
};

static ssize_t fb_read_n(int fd, unsigned char *buf, size_t n)
{
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(fd, buf + got, n - got);
        if (r <= 0)
            return -1;
        got += (size_t)r;
    }
    return (ssize_t)got;
}

/* Reads one full GemPC frame (sync-anchored like the firmware codec)
 * into out; returns frame length, or -1 on EOF/error/oversize. */
static int fb_read_frame(int fd, unsigned char *out)
{
    for (;;) {
        unsigned char b;
        if (fb_read_n(fd, &b, 1) < 0)
            return -1;
        if (b != NUC_SYNC)
            continue;
        if (fb_read_n(fd, &b, 1) < 0)
            return -1;
        if (b != NUC_CTRL_ACK)
            continue;
        out[0] = NUC_SYNC;
        out[1] = NUC_CTRL_ACK;
        if (fb_read_n(fd, out + 2, NUC_CCID_HEADER_SIZE) < 0)
            return -1;
        uint32_t dlen = (uint32_t)out[NUC_MSG_LEN_LO]
            | ((uint32_t)out[NUC_MSG_LEN_LO + 1] << 8)
            | ((uint32_t)out[NUC_MSG_LEN_LO + 2] << 16)
            | ((uint32_t)out[NUC_MSG_LEN_LO + 3] << 24);
        if (dlen > NUC_MAX_CCID_PAYLOAD)
            return -1;
        if (dlen && fb_read_n(fd, out + NUC_MSG_PAYLOAD, dlen) < 0)
            return -1;
        unsigned char lrc;
        if (fb_read_n(fd, &lrc, 1) < 0)
            return -1;
        out[NUC_MSG_PAYLOAD + dlen] = lrc;
        return (int)(NUC_MSG_PAYLOAD + dlen + 1);
    }
}

/* The firmware's EXACT interim time-extension wire bytes (#472).
 *
 * Reference-locked byte-for-byte against the emitter this driver
 * serves — crates/ccid-service/src/ccid_serial_server.rs
 * time_extension::build_frame — and its own reference-locked unit
 * test (time_extension_tests::interim_frame_bytes_reference_locked):
 *
 *   [0]  0x03  GemPC SYNC
 *   [1]  0x06  CTRL_ACK
 *   [2]  0x80  RDR_to_PC_DataBlock — CCID Rev 1.1 §6.1.4: the interim
 *              KEEPS the in-flight XfrBlock's response type
 *   [3..6]  0  dwLength = 0 (LE) — empty DataBlock
 *   [7]  0     bSlot
 *   [8]  seq   bSeq — the in-flight XfrBlock's
 *   [9]  0x80  bStatus: bmCommandStatus 0b10 = time extension requested
 *              (CCID 6.1.6) | bmICCStatus 0 = present+active
 *   [10] 0x02  bError: BWT units requested (BWT_UNIT_MULTIPLIER = 2;
 *              libccidtwin grows its read timeout by this per frame,
 *              commands.c CCID_Receive)
 *   [11] 0     bClockStatus
 *   [12] LRC   XOR of the 12 preceding bytes (calculate_lrc)
 *
 * Deliberately NOT built with nuc_build_request: that is the DRIVER's
 * builder and writes bError = 0 — synthesizing the mock with the code
 * under test is exactly the mock-models-the-driver shape #472 flags.
 * Wire size is the firmware's FRAME_SIZE = 13. */
static size_t fw_time_extension_frame(unsigned char *out, unsigned char seq)
{
    out[0] = NUC_SYNC;
    out[1] = NUC_CTRL_ACK;
    out[2] = NUC_RDR_TO_PC_DATA_BLOCK;
    out[3] = 0;
    out[4] = 0;
    out[5] = 0;
    out[6] = 0;    /* dwLength = 0 (LE) */
    out[7] = 0;    /* bSlot */
    out[8] = seq;  /* bSeq — the in-flight XfrBlock's */
    out[9] = 0x80; /* bStatus: time extension | ICC present+active */
    out[10] = 0x02; /* bError: 2 BWT units requested */
    out[11] = 0;    /* bClockStatus */
    out[12] = nuc_lrc(out, 12);
    return 13;
}

/* echo the ORIGINAL frame byte-exact, then answer with one crafted RDR
 * frame whose bSeq is the echo's seq XOR seq_xor (0 = same seq) */
static void fb_reply(int fd, const unsigned char *frame,
                     unsigned char type, unsigned char status,
                     const unsigned char *payload, uint32_t plen,
                     unsigned char seq_xor)
{
    int n = NUC_MSG_PAYLOAD + (int)nuc_resp_payload_len(frame) + 1;
    if (write(fd, frame, (size_t)n) != n) {
        close(fd);
        return;
    }
    unsigned char out[NUC_MAX_FRAME];
    size_t olen = nuc_build_request(out, type,
                                    (unsigned char)(frame[NUC_MSG_SEQ] ^ seq_xor),
                                    status, payload, plen);
    if (olen == 0) {
        close(fd);
        return;
    }
    (void)!write(fd, out, olen);
    close(fd);
}

/* complete-frame read + count, then a crafted answer (original echo) */
static void fb_read_and_reply(struct fake_board *fb, int fd,
                              unsigned char type, unsigned char status,
                              const unsigned char *payload, uint32_t plen,
                              unsigned char seq_xor)
{
    unsigned char frame[NUC_MAX_FRAME];
    if (fb_read_frame(fd, frame) <= 0) {
        close(fd);
        return;
    }
    pthread_mutex_lock(&fb->lock);
    fb->frames_complete++;
    pthread_mutex_unlock(&fb->lock);
    fb_reply(fd, frame, type, status, payload, plen, seq_xor);
}

static void fb_serve_conn(struct fake_board *fb, int fd, int step)
{
    unsigned char frame[NUC_MAX_FRAME];

    switch (step) {
    case STEP_CLOSE_EARLY:
        close(fd);
        return;
    case STEP_MIDFRAME:
        (void)!fb_read_n(fd, frame, 5);
        close(fd);
        return;
    case STEP_NO_ECHO: {
        int n = fb_read_frame(fd, frame);
        if (n > 0) {
            pthread_mutex_lock(&fb->lock);
            fb->frames_complete++;
            pthread_mutex_unlock(&fb->lock);
        }
        close(fd);
        return;
    }
    case STEP_NO_RESPONSE: {
        int n = fb_read_frame(fd, frame);
        if (n <= 0) {
            close(fd);
            return;
        }
        pthread_mutex_lock(&fb->lock);
        fb->frames_complete++;
        pthread_mutex_unlock(&fb->lock);
        if (write(fd, frame, (size_t)n) != n) {
            /* best effort: the frame was already delivered+counted */
        }
        close(fd);
        return;
    }
    case STEP_BAD_SEQ:
        /* DataBlock with SOMEONE ELSE's sequence number */
        fb_read_and_reply(fb, fd, NUC_RDR_TO_PC_DATA_BLOCK, 0,
                          (const unsigned char *)"\x90\x00", 2, 0x5A);
        return;
    case STEP_CMD_FAILED_PRESENT:
        /* bStatus: command FAILED, card PRESENT and ACTIVE
         * (bmCommandStatus=1<<6, bmICCStatus=0) */
        fb_read_and_reply(fb, fd, NUC_RDR_TO_PC_DATA_BLOCK,
                          (unsigned char)(1 << 6),
                          (const unsigned char *)"", 0, 0);
        return;
    case STEP_TIME_EXTENSION: {
        /* CCID 6.2-2 interim frame: AFTER the byte-exact echo, a
         * SlotStatus with no error (bmCommandStatus=0 = still
         * processing) and our seq — then the real DataBlock answer */
        unsigned char frame[NUC_MAX_FRAME];
        if (fb_read_frame(fd, frame) <= 0) {
            close(fd);
            return;
        }
        pthread_mutex_lock(&fb->lock);
        fb->frames_complete++;
        pthread_mutex_unlock(&fb->lock);
        int n = NUC_MSG_PAYLOAD + (int)nuc_resp_payload_len(frame) + 1;
        if (write(fd, frame, (size_t)n) != n) {
            close(fd);
            return;
        }
        unsigned char interim[NUC_MAX_FRAME];
        size_t ilen = nuc_build_request(
            interim, NUC_RDR_TO_PC_SLOT_STATUS, frame[NUC_MSG_SEQ], 0,
            (const unsigned char *)"", 0);
        if (ilen == 0 || write(fd, interim, ilen) != (ssize_t)ilen) {
            close(fd);
            return;
        }
        unsigned char answer[NUC_MAX_FRAME];
        size_t alen = nuc_build_request(
            answer, NUC_RDR_TO_PC_DATA_BLOCK, frame[NUC_MSG_SEQ], 0,
            (const unsigned char *)"\x90\x00", 2);
        if (alen != 0)
            (void)!write(fd, answer, alen);
        close(fd);
        return;
    }
    case STEP_TIME_EXTENSION_DATABLOCK: {
        /* Issue #472: the firmware's ACTUAL interim — the exact
         * build_frame bytes (fw_time_extension_frame, byte-locked
         * against ccid_serial_server's reference-locked unit test),
         * then the real answer. Pre-fix the driver only waited out
         * SlotStatus-shaped interims and classified this exchange
         * IFD_COMMUNICATION_ERROR. */
        unsigned char frame[NUC_MAX_FRAME];
        if (fb_read_frame(fd, frame) <= 0) {
            close(fd);
            return;
        }
        pthread_mutex_lock(&fb->lock);
        fb->frames_complete++;
        pthread_mutex_unlock(&fb->lock);
        int n = NUC_MSG_PAYLOAD + (int)nuc_resp_payload_len(frame) + 1;
        if (write(fd, frame, (size_t)n) != n) {
            close(fd);
            return;
        }
        unsigned char interim[NUC_MAX_FRAME];
        size_t ilen = fw_time_extension_frame(interim,
                                              frame[NUC_MSG_SEQ]);
        if (ilen == 0 || write(fd, interim, ilen) != (ssize_t)ilen) {
            close(fd);
            return;
        }
        unsigned char answer[NUC_MAX_FRAME];
        size_t alen = nuc_build_request(
            answer, NUC_RDR_TO_PC_DATA_BLOCK, frame[NUC_MSG_SEQ], 0,
            (const unsigned char *)"\x90\x00", 2);
        if (alen != 0)
            (void)!write(fd, answer, alen);
        close(fd);
        return;
    }
    case STEP_TIME_EXTENSION_CHAIN: {
        /* #472 full-chain regression: TWO XfrBlocks on ONE
         * connection; the first is answered echo -> TWO firmware
         * interims (the stall hook fires once per BWT elapse) ->
         * final DataBlock, the second echo -> final. If the driver
         * mis-consumes any interim, the leftover frame poisons the
         * NEXT transaction: its echo read gets the stale final ->
         * echo mismatch -> AMBIGUOUS + eject. */
        for (int cmd = 0; cmd < 2; cmd++) {
            unsigned char frame[NUC_MAX_FRAME];
            if (fb_read_frame(fd, frame) <= 0) {
                close(fd);
                return;
            }
            pthread_mutex_lock(&fb->lock);
            fb->frames_complete++;
            pthread_mutex_unlock(&fb->lock);
            int n = NUC_MSG_PAYLOAD + (int)nuc_resp_payload_len(frame) + 1;
            if (write(fd, frame, (size_t)n) != n) {
                close(fd);
                return;
            }
            if (cmd == 0) {
                for (int i = 0; i < 2; i++) {
                    unsigned char interim[NUC_MAX_FRAME];
                    size_t ilen = fw_time_extension_frame(
                        interim, frame[NUC_MSG_SEQ]);
                    if (write(fd, interim, ilen) != (ssize_t)ilen) {
                        close(fd);
                        return;
                    }
                }
            }
            unsigned char answer[NUC_MAX_FRAME];
            size_t alen = nuc_build_request(
                answer, NUC_RDR_TO_PC_DATA_BLOCK, frame[NUC_MSG_SEQ], 0,
                (const unsigned char *)"\x90\x00", 2);
            if (alen == 0 || write(fd, answer, alen) != (ssize_t)alen) {
                close(fd);
                return;
            }
        }
        close(fd);
        return;
    }
    case STEP_WRONG_TYPE:
        /* answers an XfrBlock with a Parameters frame */
        fb_read_and_reply(fb, fd, NUC_RDR_TO_PC_PARAMETERS, 0,
                          (const unsigned char *)"\x11\x00\x00\x0A\x00", 5, 0);
        return;
    case STEP_NONE:
    default: {
        int n = fb_read_frame(fd, frame);
        if (n <= 0) {
            close(fd);
            return;
        }
        pthread_mutex_lock(&fb->lock);
        fb->frames_complete++;
        pthread_mutex_unlock(&fb->lock);
        if (write(fd, frame, (size_t)n) != n) {
            close(fd);
            return;
        }
        /* answer: DataBlock with SW 9000, same bSeq */
        unsigned char answer[NUC_MAX_FRAME];
        size_t alen2 = nuc_build_request(
            answer, NUC_RDR_TO_PC_DATA_BLOCK, frame[NUC_MSG_SEQ], 0,
            (const unsigned char *)"\x90\x00", 2);
        if (alen2 == 0 || write(fd, answer, alen2) != (ssize_t)alen2) {
            close(fd);
            return;
        }
        close(fd);
        return;
    }
    }
}

static void *fb_thread(void *arg)
{
    struct fake_board *fb = arg;

    for (;;) {
        int fd = accept(fb->listen_fd, NULL, NULL);
        if (fd < 0)
            return NULL;
        pthread_mutex_lock(&fb->lock);
        fb->serving = 1;
        int idx = fb->conns_done; /* connection being handled now */
        int step = (idx < fb->nsteps) ? fb->steps[idx] : STEP_NONE;
        pthread_mutex_unlock(&fb->lock);
        fb_serve_conn(fb, fd, step);
        pthread_mutex_lock(&fb->lock);
        fb->conns_done++;
        fb->serving = 0;
        pthread_cond_broadcast(&fb->cv);
        pthread_mutex_unlock(&fb->lock);
    }
}

static int test_failures = 0;

static void check(int cond, const char *what)
{
    printf("%-64s %s\n", what, cond ? "PASS" : "FAIL");
    if (!cond)
        test_failures++;
}

static void wait_conns(struct fake_board *fb, int want)
{
    struct timespec ts = { time(NULL) + 5, 0 };
    pthread_mutex_lock(&fb->lock);
    while (fb->conns_done < want) {
        if (pthread_cond_timedwait(&fb->cv, &fb->lock, &ts) == ETIMEDOUT)
            break;
    }
    pthread_mutex_unlock(&fb->lock);
}

/* Reset per-scenario state: reuse the listener; script starts fresh. */
static void arm_board(struct fake_board *fb, const int *steps, int nsteps)
{
    pthread_mutex_lock(&fb->lock);
    fb->nsteps = 0;
    for (int i = 0; i < nsteps; i++)
        fb->steps[fb->nsteps++] = steps[i];
    pthread_mutex_unlock(&fb->lock);
}

static int g_frames_complete(struct fake_board *fb)
{
    pthread_mutex_lock(&fb->lock);
    int n = fb->frames_complete;
    pthread_mutex_unlock(&fb->lock);
    return n;
}

static int g_conns_done(struct fake_board *fb)
{
    pthread_mutex_lock(&fb->lock);
    int n = fb->conns_done;
    pthread_mutex_unlock(&fb->lock);
    return n;
}

static const unsigned char apdu[] = { 0x00, 0xA4, 0x04, 0x00 };

static RESPONSECODE ifd_transmit_once(struct fake_board *fb, int step,
                                      unsigned char *rx, DWORD rxsz,
                                      DWORD *rxlen)
{
    int steps[2] = { step, STEP_NONE };
    arm_board(fb, steps, 2);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);
    SCARD_IO_HEADER send_pci = { 0, 0 }, recv_pci = { 0, 0 };
    unsigned char tx[4];
    memcpy(tx, apdu, sizeof(tx));
    *rxlen = rxsz;
    return IFDHTransmitToICC(0, send_pci, tx, sizeof(tx), rx,
                             rxlen, &recv_pci);
}

/* Wait until every accepted connection is served AND counted — after
 * this, counter resets cannot race a late increment from the board
 * thread (the flake where the next connection applied steps[1]). */
static void fb_quiesce(struct fake_board *fb)
{
    struct timespec ts = { time(NULL) + 5, 0 };
    pthread_mutex_lock(&fb->lock);
    while (fb->serving) {
        if (pthread_cond_timedwait(&fb->cv, &fb->lock, &ts) == ETIMEDOUT)
            break;
    }
    pthread_mutex_unlock(&fb->lock);
}

static void reset_between(struct fake_board *fb)
{
    fb_quiesce(fb);
    pthread_mutex_lock(&fb->lock);
    fb->frames_complete = 0;
    fb->conns_done = 0;
    pthread_mutex_unlock(&fb->lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);
}

int main(void)
{
    struct fake_board fb = { .lock = PTHREAD_MUTEX_INITIALIZER,
                             .cv = PTHREAD_COND_INITIALIZER };
    unsigned char resp[NUC_MAX_FRAME];
    int rn = 0;
    int rc;

    setvbuf(stdout, NULL, _IOLBF, 0);
    setvbuf(stderr, NULL, _IOLBF, 0);
    signal(SIGPIPE, SIG_IGN); /* the board side writes to dead sockets */
    printf("== ifd-nucula tx-semantics fault-injection test ==\n");

    fb.listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fb.listen_fd < 0) {
        perror("socket");
        return 2;
    }
    int one = 1;
    setsockopt(fb.listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in sa = { .sin_family = AF_INET,
                              .sin_addr.s_addr = htonl(INADDR_LOOPBACK) };
    socklen_t alen = sizeof(sa);
    if (bind(fb.listen_fd, (struct sockaddr *)&sa, sizeof(sa)) != 0 ||
        getsockname(fb.listen_fd, (struct sockaddr *)&sa, &alen) != 0 ||
        listen(fb.listen_fd, 4) != 0) {
        perror("bind/listen");
        return 2;
    }

    /* point the driver's global ctx at the fake board (white-box) */
    snprintf(g_ctx.host, sizeof(g_ctx.host), "127.0.0.1");
    g_ctx.port = ntohs(sa.sin_port);
    g_ctx.fd = -1;

    pthread_t tid;
    if (pthread_create(&tid, NULL, fb_thread, &fb) != 0) {
        perror("pthread_create");
        return 2;
    }

    /* -- scenario 1: baseline (no fault) — recovers, 1 frame -- */
    {
        int steps[] = { STEP_NONE, STEP_NONE, STEP_NONE };
        arm_board(&fb, steps, 3);
        rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                         resp, &rn);
        wait_conns(&fb, 1);
        check(rc == NUC_TX_OK, "baseline: exchange completes (TX_OK)");
        check(g_frames_complete(&fb) == 1,
              "baseline: exactly one frame reached the board");
    }
    pthread_mutex_lock(&fb.lock);
    fb.frames_complete = 0;
    fb.conns_done = 0;
    pthread_mutex_unlock(&fb.lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);

    /* -- scenario 2: peer dies mid-read (midframe cut) -- */
    {
        int steps[] = { STEP_MIDFRAME, STEP_NONE, STEP_NONE };
        arm_board(&fb, steps, 3);
        rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                         resp, &rn);
        wait_conns(&fb, 1);
        check(rc == NUC_TX_AMBIGUOUS,
              "midframe cut: classified AMBIGUOUS (no resend)");
        check(g_frames_complete(&fb) == 0,
              "midframe cut: frame never completed at the peer");
        check(g_conns_done(&fb) == 1,
              "midframe cut: no second connection opened");
    }
    pthread_mutex_lock(&fb.lock);
    fb.frames_complete = 0;
    fb.conns_done = 0;
    pthread_mutex_unlock(&fb.lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);

    /* -- scenario 3: connection dies before any byte (close-early) --
     * same reasoning as scenario 2: local write success makes this
     * indistinguishable from post-delivery death */
    {
        int steps[] = { STEP_CLOSE_EARLY, STEP_NONE, STEP_NONE };
        arm_board(&fb, steps, 3);
        rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                         resp, &rn);
        wait_conns(&fb, 1);
        struct timespec pause = { 0, 200 * 1000 * 1000 };
        nanosleep(&pause, NULL);
        check(rc != NUC_TX_OK,
              "close-early: driver reports failure, not success");
        check(g_conns_done(&fb) == 1,
              "close-early: no second connection opened");
    }
    pthread_mutex_lock(&fb.lock);
    fb.frames_complete = 0;
    fb.conns_done = 0;
    pthread_mutex_unlock(&fb.lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);

    /* -- scenario 3b: TRUE send failure (write errors) — safe retry.
     * A dead-write socketpair injected into the ctx makes the first
     * send fail; the retry must eject, reconnect to the fake board,
     * and complete exactly one exchange. */
    {
        int sv[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
            check(0, "socketpair created");
        } else {
            int steps[] = { STEP_NONE, STEP_NONE, STEP_NONE };
            arm_board(&fb, steps, 3);
            /* peer refuses to read ever again: our first send errors
             * (EPIPE/ECONNRESET with MSG_NOSIGNAL) */
            shutdown(sv[1], SHUT_RD);
            pthread_mutex_lock(&g_ctx.lock);
            g_ctx.fd = sv[0]; /* pretend we are "connected" to a corpse */
            pthread_mutex_unlock(&g_ctx.lock);
            rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                             resp, &rn);
            wait_conns(&fb, 1);
            check(rc == NUC_TX_OK,
                  "send-failure: provably-not-sent retry recovers (TX_OK)");
            check(g_frames_complete(&fb) == 1,
                  "send-failure: exactly one complete frame (the retry)");
            check(g_conns_done(&fb) == 1,
                  "send-failure: one new connection for the retry");
            close(sv[0]);
            close(sv[1]);
        }
    }
    pthread_mutex_lock(&fb.lock);
    fb.frames_complete = 0;
    fb.conns_done = 0;
    pthread_mutex_unlock(&fb.lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);

    /* -- scenario 4: full frame delivered, echo lost (NO_ECHO) --
     * THE duplicate-APDU case: retry forbidden */
    {
        int steps[] = { STEP_NO_ECHO, STEP_NONE, STEP_NONE };
        arm_board(&fb, steps, 3);
        rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                         resp, &rn);
        wait_conns(&fb, 1);
        /* give the driver's failure path a moment in case it (wrongly)
         * opens a second connection */
        struct timespec pause = { 0, 200 * 1000 * 1000 };
        nanosleep(&pause, NULL);
        check(rc != NUC_TX_OK, "no-echo: driver reports failure, not success");
        check(rc == NUC_TX_AMBIGUOUS, "no-echo: outcome classified AMBIGUOUS");
        check(g_frames_complete(&fb) == 1,
              "no-echo: exactly ONE frame delivered (no duplicate)");
        check(g_conns_done(&fb) == 1,
              "no-echo: no second connection opened");
    }
    pthread_mutex_lock(&fb.lock);
    fb.frames_complete = 0;
    fb.conns_done = 0;
    pthread_mutex_unlock(&fb.lock);
    pthread_mutex_lock(&g_ctx.lock);
    nuc_eject_locked(&g_ctx);
    pthread_mutex_unlock(&g_ctx.lock);

    /* -- scenario 5: echo verified, response lost (NO_RESPONSE) --
     * card may have executed: retry forbidden */
    {
        int steps[] = { STEP_NO_RESPONSE, STEP_NONE, STEP_NONE };
        arm_board(&fb, steps, 3);
        rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                         resp, &rn);
        wait_conns(&fb, 1);
        struct timespec pause = { 0, 200 * 1000 * 1000 };
        nanosleep(&pause, NULL);
        check(rc != NUC_TX_OK,
              "no-response: driver reports failure, not success");
        check(rc == NUC_TX_AMBIGUOUS,
              "no-response: outcome classified AMBIGUOUS");
        check(g_frames_complete(&fb) == 1,
              "no-response: exactly ONE frame delivered (no duplicate)");
        check(g_conns_done(&fb) == 1,
              "no-response: no second connection opened");
    }

    /* -- scenario 6: response-decode semantics (Task 1.4) -- */
    printf("-- response decode: CCID status/type/seq handling --\n");
    {
        unsigned char rx[NUC_MAX_FRAME];
        DWORD rxlen;
        RESPONSECODE r;

        /* command FAILED with card PRESENT+ACTIVE must NOT be reported
         * as a successful IFD exchange (pre-fix: IFD_SUCCESS + payload) */
        reset_between(&fb);
        r = ifd_transmit_once(&fb, STEP_CMD_FAILED_PRESENT, rx, sizeof(rx),
                              &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_COMMUNICATION_ERROR,
              "cmd-failed+present: IFD_COMMUNICATION_ERROR (not success)");
        check(rxlen == 0, "cmd-failed+present: no payload returned");
        check(g_frames_complete(&fb) == 1, "cmd-failed: no resend");

        /* CCID 6.2-2 time extension: an interim in-progress SlotStatus
         * must be waited out, not treated as the final answer */
        reset_between(&fb);
        r = ifd_transmit_once(&fb, STEP_TIME_EXTENSION, rx, sizeof(rx),
                              &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_SUCCESS, "time-extension: exchange still succeeds");
        check(rxlen == 2 && rx[0] == 0x90 && rx[1] == 0x00,
              "time-extension: final DataBlock payload delivered");
        check(g_frames_complete(&fb) == 1, "time-extension: no resend");

        /* #472: the firmware's DataBlock-typed 6.1.4 interim must be
         * waited out exactly like the 6.2-2 SlotStatus interim */
        reset_between(&fb);
        r = ifd_transmit_once(&fb, STEP_TIME_EXTENSION_DATABLOCK, rx,
                              sizeof(rx), &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_SUCCESS,
              "datablock-interim: exchange still succeeds");
        check(rxlen == 2 && rx[0] == 0x90 && rx[1] == 0x00,
              "datablock-interim: final DataBlock payload delivered");
        check(g_frames_complete(&fb) == 1, "datablock-interim: no resend");

        /* #472 full-chain regression: XfrBlock -> extension
         * DataBlock(s) -> final DataBlock returns the FINAL frame's
         * payload with rc=0, and the NEXT transaction on the SAME
         * connection is not poisoned (pre-fix, the interim ended the
         * wait early, the real final frame stayed in flight, and the
         * next echo read stale bytes -> AMBIGUOUS + eject). */
        reset_between(&fb);
        {
            int steps[] = { STEP_TIME_EXTENSION_CHAIN, STEP_NONE };
            arm_board(&fb, steps, 2);
            pthread_mutex_lock(&g_ctx.lock);
            unsigned char seq0 = g_ctx.seq;
            pthread_mutex_unlock(&g_ctx.lock);

            rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                             resp, &rn);
            check(rc == NUC_TX_OK,
                  "ext-chain: stalled XfrBlock completes (TX_OK)");
            check(rn == NUC_MSG_PAYLOAD + 3
                      && resp[NUC_MSG_PAYLOAD] == 0x90
                      && resp[NUC_MSG_PAYLOAD + 1] == 0x00,
                  "ext-chain: the FINAL frame's payload delivered (9000)");
            check(resp[NUC_MSG_SEQ] == seq0,
                  "ext-chain: final answers the stalled command's bSeq");

            pthread_mutex_lock(&g_ctx.lock);
            int fd1 = g_ctx.fd;
            pthread_mutex_unlock(&g_ctx.lock);
            check(fd1 >= 0, "ext-chain: connection alive after interims");

            rc = nuc_command(NUC_PC_TO_RDR_XFR_BLOCK, 0, apdu, sizeof(apdu),
                             resp, &rn);
            wait_conns(&fb, 1);
            check(rc == NUC_TX_OK,
                  "ext-chain: NEXT transaction not poisoned (TX_OK)");
            check(rn == NUC_MSG_PAYLOAD + 3
                      && resp[NUC_MSG_PAYLOAD] == 0x90
                      && resp[NUC_MSG_PAYLOAD + 1] == 0x00,
                  "ext-chain: next transaction's payload delivered");
            check(resp[NUC_MSG_SEQ] == (unsigned char)(seq0 + 1),
                  "ext-chain: next bSeq is seq+1 (counter clean)");

            pthread_mutex_lock(&g_ctx.lock);
            int fd2 = g_ctx.fd;
            pthread_mutex_unlock(&g_ctx.lock);
            check(fd2 == fd1, "ext-chain: same connection reused (no eject)");
            check(g_frames_complete(&fb) == 2,
                  "ext-chain: exactly two frames, no resend");
            check(g_conns_done(&fb) == 1,
                  "ext-chain: both transactions on ONE connection");
        }

        /* wrong bSeq: correlation failure — error, and the poisoned
         * connection must be dropped (next command reconnects) */
        reset_between(&fb);
        r = ifd_transmit_once(&fb, STEP_BAD_SEQ, rx, sizeof(rx), &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_COMMUNICATION_ERROR, "bad-seq: IFD_COMMUNICATION_ERROR");
        check(g_frames_complete(&fb) == 1, "bad-seq: no resend");
        r = ifd_transmit_once(&fb, STEP_NONE, rx, sizeof(rx), &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_SUCCESS,
              "bad-seq recovery: fresh connection works after eject");

        /* wrong response type for an XfrBlock: error, no payload */
        reset_between(&fb);
        r = ifd_transmit_once(&fb, STEP_WRONG_TYPE, rx, sizeof(rx), &rxlen);
        wait_conns(&fb, 1);
        check(r == IFD_COMMUNICATION_ERROR,
              "wrong-type: Parameters frame to XfrBlock rejected");
        check(rxlen == 0, "wrong-type: no payload returned");
    }

    /* wake the accept-blocked board thread (close() alone does not),
     * then join */
    shutdown(fb.listen_fd, SHUT_RDWR);
    close(fb.listen_fd);
    pthread_join(tid, NULL);

    printf("== %s (%d failures) ==\n",
           test_failures ? "FAIL" : "ALL PASS", test_failures);
    return test_failures ? 1 : 0;
}
