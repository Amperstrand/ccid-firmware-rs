/*
 * bounds_test.c — frame-buffer boundary reproducer/regression for
 * host-tools/ifd-nucula (hardening WP H-001, Task 1.1).
 *
 * Compiles ifd-nucula.c in-process (statics included) under
 * ASan+UBSan and exercises the frame codec at the exact wire
 * boundaries: payload/dwLength 0, 1, 260, 261 (must succeed) and
 * 262, 271, 512 (must be rejected without touching memory past the
 * 274-byte frame buffers).
 *
 * Reference model (authoritative, reference-locked in-tree):
 *   crates/ccid-protocol/src/types.rs      MAX_CCID_MESSAGE_LENGTH = 271
 *                                         (a CCID bulk message = 10-byte
 *                                          header + payload, capped 271)
 *   crates/ccid-transport-serial/src/lib.rs MAX_CCID_PAYLOAD = 261
 *                                          MAX_FRAME_BYTES = 2+271+1 = 274
 *                                          header-stage reject of
 *                                          payload_len > 261
 *
 * Build/run:  make bounds      (CC=clang works; gcc ASan fine too)
 *
 * Verdict semantics: every case prints PASS/FAIL; the binary exits 0
 * only if every case passed. Pre-fix, the first oversize case aborts
 * with an AddressSanitizer stack-buffer-overflow report — that abort
 * IS the reproduction (RED); it must disappear with the fix.
 *
 * License: GPL-2.0-or-later (same as the repo).
 */

#include "ifd-nucula.c"

#include <sys/socket.h>
#include <sys/wait.h>

/* pcscd's log_msg() is not linkable outside the daemon; mirror the
 * debuglog.h prototype so the driver's logging calls resolve here. */
void log_msg(const int priority, const char *fmt, ...)
{
    va_list ap;
    fprintf(stderr, "[pcsc:%d] ", priority);
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}

static int failures = 0;

static void check(int cond, const char *what)
{
    printf("%-64s %s\n", what, cond ? "PASS" : "FAIL");
    if (!cond)
        failures++;
}

/* Forked per group so a pre-fix ASan abort in one group (the RED
 * evidence) still lets the others report. Child exit: 0 clean, 1 FAILs,
 * signal = sanitizer killed it (overflow reproduced). */
static void run_group(const char *name, void (*fn)(void))
{
    printf("== group: %s ==\n", name);
    fflush(stdout);
    pid_t pid = fork();
    if (pid == 0) {
        fn();
        fflush(stdout);
        _exit(failures ? 1 : 0);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    if (WIFEXITED(st) && WEXITSTATUS(st) == 0)
        printf("== group: %s — OK ==\n\n", name);
    else if (WIFEXITED(st) && WEXITSTATUS(st) == 1)
        printf("== group: %s — FAIL (assertions) ==\n\n", name);
    else if (WIFSIGNALED(st))
        printf("== group: %s — ABORTED by signal %d "
               "(sanitizer caught the overflow: RED) ==\n\n", name,
               WTERMSIG(st));
    else
        printf("== group: %s — UNKNOWN status ==\n\n", name);
    if (!(WIFEXITED(st) && WEXITSTATUS(st) == 0))
        failures++;
}

/* ---- TX path: nuc_build_request ---- */

static void test_build_request_boundaries(void)
{
    static unsigned char payload[600];
    unsigned char frame[NUC_MAX_FRAME]; /* exact production buffer size */

    for (size_t i = 0; i < sizeof(payload); i++)
        payload[i] = (unsigned char)(i * 7 + 1);

    printf("-- nuc_build_request: accepted payload sizes --\n");
    static const uint32_t ok_lens[] = { 0, 1, 260, 261 };
    for (size_t t = 0; t < sizeof(ok_lens) / sizeof(ok_lens[0]); t++) {
        uint32_t plen = ok_lens[t];
        memset(frame, 0xAA, sizeof(frame));
        size_t n = nuc_build_request(frame, NUC_PC_TO_RDR_XFR_BLOCK, 7, 0,
                                     payload, plen);
        char what[96];
        snprintf(what, sizeof(what),
                 "build_request plen=%u returns full frame len %zu",
                 plen, (size_t)(NUC_MSG_PAYLOAD + plen + 1));
        check(n == NUC_MSG_PAYLOAD + plen + 1, what);
        if (n != NUC_MSG_PAYLOAD + plen + 1)
            continue;
        snprintf(what, sizeof(what), "build_request plen=%u framing+LRC valid",
                 plen);
        int lrc_ok = frame[n - 1] == nuc_lrc(frame, n - 1);
        int payload_ok = plen == 0 ||
            memcmp(frame + NUC_MSG_PAYLOAD, payload, plen) == 0;
        check(lrc_ok && payload_ok &&
              frame[0] == NUC_SYNC && frame[1] == NUC_CTRL_ACK &&
              frame[NUC_MSG_SEQ] == 7, what);
    }

    printf("-- nuc_build_request: rejected payload sizes "
           "(pre-fix: ASan abort here) --\n");
    static const uint32_t bad_lens[] = { 262, 271, 512 };
    for (size_t t = 0; t < sizeof(bad_lens) / sizeof(bad_lens[0]); t++) {
        uint32_t plen = bad_lens[t];
        size_t n = nuc_build_request(frame, NUC_PC_TO_RDR_XFR_BLOCK, 7, 0,
                                     payload, plen);
        char what[96];
        snprintf(what, sizeof(what),
                 "build_request plen=%u refused (0), no overflow", plen);
        check(n == 0, what);
    }
}

/* ---- RX path: nuc_recv_frame_locked over a socketpair ---- */

static size_t craft_frame(unsigned char *out, uint32_t dwlen)
{
    /* header: type SLOT_STATUS, dwLength=dwlen, seq 7 */
    out[0] = NUC_SYNC;
    out[1] = NUC_CTRL_ACK;
    out[NUC_MSG_TYPE] = NUC_RDR_TO_PC_SLOT_STATUS;
    out[NUC_MSG_LEN_LO] = (unsigned char)(dwlen & 0xFF);
    out[NUC_MSG_LEN_LO + 1] = (unsigned char)((dwlen >> 8) & 0xFF);
    out[NUC_MSG_LEN_LO + 2] = (unsigned char)((dwlen >> 16) & 0xFF);
    out[NUC_MSG_LEN_LO + 3] = (unsigned char)((dwlen >> 24) & 0xFF);
    out[7] = 0;
    out[NUC_MSG_SEQ] = 7;
    out[NUC_MSG_STATUS] = 0;
    out[NUC_MSG_ERROR] = 0;
    out[11] = 0;
    for (uint32_t i = 0; i < dwlen; i++)
        out[NUC_MSG_PAYLOAD + i] = (unsigned char)(i * 3 + 5);
    size_t body = NUC_MSG_PAYLOAD + dwlen;
    out[body] = nuc_lrc(out, body);
    return body + 1;
}

static void feed_and_recv(uint32_t dwlen, int expect_ok)
{
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        check(0, "socketpair created");
        return;
    }
    unsigned char wire[1024];
    size_t wlen = craft_frame(wire, dwlen);
    if (write(sv[1], wire, wlen) != (ssize_t)wlen) {
        check(0, "socketpair feed write");
        close(sv[0]);
        close(sv[1]);
        return;
    }

    nuc_ctx c = { .fd = sv[0] };
    unsigned char frame[NUC_MAX_FRAME]; /* exact production buffer size */
    memset(frame, 0xAA, sizeof(frame));
    int n = nuc_recv_frame_locked(&c, frame);

    char what[96];
    if (expect_ok) {
        snprintf(what, sizeof(what),
                 "recv dwlen=%u accepted with frame len %zu", dwlen,
                 (size_t)(NUC_MSG_PAYLOAD + dwlen + 1));
        int ok = n == (int)(NUC_MSG_PAYLOAD + dwlen + 1);
        if (ok)
            ok = dwlen == 0 ||
                 memcmp(frame + NUC_MSG_PAYLOAD, wire + NUC_MSG_PAYLOAD,
                        dwlen) == 0;
        check(ok, what);
    } else {
        snprintf(what, sizeof(what),
                 "recv dwlen=%u rejected (-1), no overflow", dwlen);
        check(n == -1, what);
    }
    close(sv[0]);
    close(sv[1]);
}

static void test_recv_frame_boundaries(void)
{
    printf("-- nuc_recv_frame_locked: accepted dwLength sizes --\n");
    static const uint32_t ok_lens[] = { 0, 1, 260, 261 };
    for (size_t t = 0; t < sizeof(ok_lens) / sizeof(ok_lens[0]); t++)
        feed_and_recv(ok_lens[t], 1);

    printf("-- nuc_recv_frame_locked: rejected dwLength sizes "
           "(pre-fix: ASan abort here) --\n");
    static const uint32_t bad_lens[] = { 262, 271, 512 };
    for (size_t t = 0; t < sizeof(bad_lens) / sizeof(bad_lens[0]); t++)
        feed_and_recv(bad_lens[t], 0);
}

/* ---- IFD entry-point guards (no board needed: reject happens first) ---- */

static void test_ifd_entry_guards(void)
{
    static unsigned char tx[600];
    unsigned char rx[NUC_MAX_FRAME];
    DWORD rxlen = sizeof(rx);
    SCARD_IO_HEADER send_pci = { 0, 0 }, recv_pci = { 0, 0 };

    for (size_t i = 0; i < sizeof(tx); i++)
        tx[i] = (unsigned char)(i + 1);

    printf("-- IFDHTransmitToICC oversize rejection --\n");
    static const DWORD bad_tx[] = { 262, 271, 512 };
    for (size_t t = 0; t < sizeof(bad_tx) / sizeof(bad_tx[0]); t++) {
        rxlen = sizeof(rx);
        RESPONSECODE r = IFDHTransmitToICC(0, send_pci, tx, bad_tx[t], rx,
                                           &rxlen, &recv_pci);
        char what[96];
        snprintf(what, sizeof(what),
                 "IFDHTransmitToICC TxLength=%lu refused, RxLength zeroed",
                 (unsigned long)bad_tx[t]);
        check(r == IFD_COMMUNICATION_ERROR && rxlen == 0, what);
    }

    printf("-- IFDHControl (CCID escape) oversize rejection --\n");
    DWORD ctl = (DWORD)SCARD_CTL_CODE(FEATURE_CCID_ESC_COMMAND);
    for (size_t t = 0; t < sizeof(bad_tx) / sizeof(bad_tx[0]); t++) {
        DWORD nret = 0;
        RESPONSECODE r = IFDHControl(0, ctl, tx, bad_tx[t], rx, sizeof(rx),
                                     &nret);
        char what[96];
        snprintf(what, sizeof(what),
                 "IFDHControl escape TxLength=%lu refused", (unsigned long)bad_tx[t]);
        check(r == IFD_COMMUNICATION_ERROR && nret == 0, what);
    }
}

/* ---- ATR walker: nuc_t1_from_atr must stay inside atr[0..len) ---- */

static void test_t1_walker_boundaries(void)
{
    printf("-- nuc_t1_from_atr: well-formed reference --\n");
    {
        static const unsigned char g05[] = {
            0x3B, 0x85, 0x81, 0x31, 0xFE, 0x45,
            0x80, 0x73, 0xC8, 0x21, 0x10, 0x84
        };
        unsigned char ifsc = 0, wi = 0;
        nuc_t1_from_atr(g05, sizeof(g05), &ifsc, &wi);
        check(ifsc == 0xFE && wi == 0x45,
              "t1_from_atr G-05 ATR -> IFSC 0xFE, WI 0x45");
    }

    printf("-- nuc_t1_from_atr: truncated/walking-edge ATRs "
           "(pre-fix: ASan abort here) --\n");
    {
        unsigned char atr[3] = { 0x3B, 0xFF, 0xAA };
        unsigned char ifsc = 0, wi = 0;
        nuc_t1_from_atr(atr, sizeof(atr), &ifsc, &wi);
        check(ifsc == 0xFE && wi == 0x45,
              "t1_from_atr len=3 all-flags-promised stops at ATR end");
    }
    {
        unsigned char atr[MAX_ATR_SIZE];
        atr[0] = 0x3B;
        atr[1] = 0xFF;
        for (size_t i = 2; i < MAX_ATR_SIZE; i++)
            atr[i] = 0xFF;
        unsigned char ifsc = 0, wi = 0;
        nuc_t1_from_atr(atr, MAX_ATR_SIZE, &ifsc, &wi);
        check(1, "t1_from_atr full-size edge-walking ATR stays in bounds");
    }
}

int main(void)
{
    printf("== ifd-nucula bounds test (ASan+UBSan build) ==\n");
    run_group("nuc_build_request boundaries", test_build_request_boundaries);
    run_group("nuc_recv_frame_locked boundaries", test_recv_frame_boundaries);
    run_group("IFD entry-point guards", test_ifd_entry_guards);
    run_group("nuc_t1_from_atr boundaries", test_t1_walker_boundaries);
    printf("== %s (%d failures) ==\n", failures ? "FAIL" : "ALL PASS",
           failures);
    return failures ? 1 : 0;
}
