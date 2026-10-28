/**
  ******************************************************************************
  * @file    usbd_ccid.c
  * @author  MCD Application Team
  * @brief   This file provides the high layer firmware functions to manage
  *          all the functionalities of the USB CCID Class:
  *
  ******************************************************************************
  * @attention
  *
  * Copyright (c) 2021 STMicroelectronics.
  * All rights reserved.
  *
  * This software is licensed under terms that can be found in the LICENSE file
  * in the root directory of this software component.
  * If no LICENSE file comes with this software, it is provided AS-IS.
  *
  ******************************************************************************
  *  @verbatim
  *
  *          ===================================================================
  *                                CCID Class Driver Description
/* ==========================================================================
   TRIMMED QUOTE-ONLY COPY (ccid-firmware-rs, issue #138) — never compiled.
   Kept: the bulk-OUT transfer state machine (CCID_STATE_RECEIVE_DATA /
   short-packet vs exact-MaxPcktLen completion vs ZLP handling) and the
   bulk-IN send path. Removed: init/deinit boilerplate, EP0 control,
   dispatch table bodies. Upstream pin: reference/ST/PROVENANCE.md.
   ========================================================================== */

  pdev->pClassDataCmsit[pdev->classId] = (void *)hccid;
  pdev->pClassData = pdev->pClassDataCmsit[pdev->classId];

#ifdef USE_USBD_COMPOSITE
  /* Get the Endpoints addresses allocated for this class instance */
  CCIDInEpAdd  = USBD_CoreGetEPAdd(pdev, USBD_EP_IN, USBD_EP_TYPE_BULK, (uint8_t)pdev->classId);
  CCIDOutEpAdd = USBD_CoreGetEPAdd(pdev, USBD_EP_OUT, USBD_EP_TYPE_BULK, (uint8_t)pdev->classId);
  CCIDCmdEpAdd = USBD_CoreGetEPAdd(pdev, USBD_EP_IN, USBD_EP_TYPE_INTR, (uint8_t)pdev->classId);
#endif /* USE_USBD_COMPOSITE */

  /* Init the CCID parameters into a state where it can receive a new command message */
  hccid->USBD_CCID_Param.bAbortRequestFlag = 0U;
  hccid->USBD_CCID_Param.bSeq = 0U;
  hccid->USBD_CCID_Param.bSlot = 0U;
  hccid->MaxPcktLen = (pdev->dev_speed == USBD_SPEED_HIGH) ? \
                      CCID_DATA_HS_MAX_PACKET_SIZE : CCID_DATA_FS_MAX_PACKET_SIZE;

  /* Open EP IN */
  (void)USBD_LL_OpenEP(pdev, CCIDInEpAdd, USBD_EP_TYPE_BULK, (uint16_t)hccid->MaxPcktLen);
  pdev->ep_in[CCIDInEpAdd & 0xFU].is_used = 1U;

  /* Open EP OUT */
  (void)USBD_LL_OpenEP(pdev, CCIDOutEpAdd, USBD_EP_TYPE_BULK, (uint16_t)hccid->MaxPcktLen);
  pdev->ep_out[CCIDOutEpAdd & 0xFU].is_used = 1U;

  /* Open INTR EP IN */
  (void)USBD_LL_OpenEP(pdev, CCIDCmdEpAdd,
                       USBD_EP_TYPE_INTR, CCID_CMD_PACKET_SIZE);
  pdev->ep_in[CCIDCmdEpAdd & 0xFU].is_used = 1U;

  /* Init  physical Interface components */
  ((USBD_CCID_ItfTypeDef *)pdev->pUserData[pdev->classId])->Init(pdev);

  /* Prepare Out endpoint to receive next packet */
  (void)USBD_LL_PrepareReceive(pdev, CCIDOutEpAdd,
                               hccid->data, hccid->MaxPcktLen);

  return (uint8_t)USBD_OK;
}

/**
  * @brief  USBD_CCID_DeInit
  *         DeInitialize the CCID layer
  * @param  pdev: device instance
  * @param  cfgidx: Configuration index
  * @retval status
  */
static uint8_t USBD_CCID_DeInit(USBD_HandleTypeDef *pdev, uint8_t cfgidx)
{
  UNUSED(cfgidx);

/* ... TRIMMED ... */


          /* Fill CCID_BulkOut Data Buffer from USB Buffer */
          (void)USBD_CCID_ReceiveCmdHeader(pdev, (uint8_t *)&hccid->UsbBlkOutData.bMessageType,
                                           (uint16_t)CurrPcktLen);

          /*
          Refer : 6 CCID Messages
          The response messages always contain the exact same slot number,
          and sequence number fields from the header that was contained in
          the Bulk-OUT command message.
          */
          hccid->UsbBlkInData.bSlot = hccid->UsbBlkOutData.bSlot;
          hccid->UsbBlkInData.bSeq = hccid->UsbBlkOutData.bSeq;

          if (CurrPcktLen < hccid->MaxPcktLen)
          {
            /* Short message, less than the EP Out Size, execute the command,
             if parameter like dwLength is too big, the appropriate command will
             give an error */
            (void)USBD_CCID_DispatchCommand(pdev);
          }
          else
          {
            /* Check if length of data to be sent by host is > buffer size */
            if (hccid->UsbBlkOutData.dwLength > (uint32_t)ABDATA_SIZE)
            {
              /* Too long data received.... Error ! */
              hccid->blkt_state = CCID_STATE_UNCORRECT_LENGTH;
            }

            else
            {
              /* Expect more data on OUT EP */
              hccid->blkt_state = CCID_STATE_RECEIVE_DATA;

              /* Prepare EP to Receive next Cmd */
              (void)USBD_LL_PrepareReceive(pdev, CCID_OUT_EP,
                                           hccid->data, hccid->MaxPcktLen);

            } /* if (CurrPcktLen == CCID_DATA_MAX_PACKET_SIZE) ends */
          } /*  if (CurrPcktLen >= CCID_DATA_MAX_PACKET_SIZE) ends */
        } /* if (CurrPcktLen >= CCID_CMD_HEADER_SIZE) ends */
        else
        {
          if (CurrPcktLen == 0x00U) /* Zero Length Packet Received */
          {
            hccid->blkt_state = CCID_STATE_IDLE;
          }
        }

        break;

      case CCID_STATE_RECEIVE_DATA:
        hccid->UsbMessageLength += CurrPcktLen;

        if (CurrPcktLen < hccid->MaxPcktLen)
        {
          /* Short message, less than the EP Out Size, execute the command,
           if parameter like dwLength is too big, the appropriate command will
           give an error */

          /* Full command is received, process the Command */
          (void)USBD_CCID_ReceiveCmdHeader(pdev, (uint8_t *)&hccid->UsbBlkOutData.bMessageType,
                                           (uint16_t)CurrPcktLen);

          (void)USBD_CCID_DispatchCommand(pdev);
        }
        else if (CurrPcktLen == hccid->MaxPcktLen)
        {
          if (hccid->UsbMessageLength < (hccid->UsbBlkOutData.dwLength + (uint32_t)CCID_CMD_HEADER_SIZE))
          {
            (void)USBD_CCID_ReceiveCmdHeader(pdev, (uint8_t *)&hccid->UsbBlkOutData.bMessageType,
                                             (uint16_t)CurrPcktLen); /* Copy data */

            /* Prepare EP to Receive next Cmd */
            (void)USBD_LL_PrepareReceive(pdev, CCID_OUT_EP,
                                         hccid->data, hccid->MaxPcktLen);
          }
          else if (hccid->UsbMessageLength == (hccid->UsbBlkOutData.dwLength + (uint32_t)CCID_CMD_HEADER_SIZE))
          {
            /* Full command is received, process the Command */
            (void)USBD_CCID_ReceiveCmdHeader(pdev, (uint8_t *)&hccid->UsbBlkOutData.bMessageType,
                                             (uint16_t)CurrPcktLen);

            (void)USBD_CCID_DispatchCommand(pdev);
          }
          else
          {
            /* Too long data received.... Error ! */
            hccid->blkt_state = CCID_STATE_UNCORRECT_LENGTH;
          }
        }
        else
        {
          /* Too long data received.... Error ! */
          hccid->blkt_state = CCID_STATE_UNCORRECT_LENGTH;
/* ... TRIMMED ... */
