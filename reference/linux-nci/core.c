// SPDX-License-Identifier: GPL-2.0-only
/*
 *  The NFC Controller Interface is the communication protocol between an
 *  NFC Controller (NFCC) and a Device Host (DH).
 *
 *  Copyright (C) 2011 Texas Instruments, Inc.
 *  Copyright (C) 2014 Marvell International Ltd.
 *
 *  Written by Ilan Elias <ilane@ti.com>
 *
 *  Acknowledgements:
 *  This file is based on hci_core.c, which was written
 *  by Maxim Krasnyansky.
 */
/*
 * ---------------------------------------------------------------------------
 * QUOTE-ONLY COPY (never compiled, never linked) — TRIMMED excerpt.
 * Full file: net/nfc/nci/core.c at the commit pinned in
 * reference/linux-nci/PROVENANCE.md. Kept region: the nci_rx_work
 * RX-dispatch switch on MT (the quote behind pn7160-nci's Frame::decode
 * RFU-MT rejection).
 * ---------------------------------------------------------------------------
 */


		/* Process frame */
		switch (nci_mt(skb->data)) {
		case NCI_MT_RSP_PKT:
			nci_rsp_packet(ndev, skb);
			break;

		case NCI_MT_NTF_PKT:
			nci_ntf_packet(ndev, skb);
			break;

		case NCI_MT_DATA_PKT:
			nci_rx_data_packet(ndev, skb);
			break;

		default:
			pr_err("unknown MT 0x%x\n", nci_mt(skb->data));
			kfree_skb(skb);
			break;
		}
