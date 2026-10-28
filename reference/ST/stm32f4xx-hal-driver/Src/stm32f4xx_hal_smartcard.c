/**
  ******************************************************************************
  * @file    stm32f4xx_hal_smartcard.c
  * @author  MCD Application Team
  * @brief   SMARTCARD HAL module driver.
  *          This file provides firmware functions to manage the following
  *          functionalities of the SMARTCARD peripheral:
  *           + Initialization and de-initialization functions
  *           + IO operation functions
  *           + Peripheral Control functions
  *           + Peripheral State and Error functions
  *
  ******************************************************************************
  * @attention
  *
  * Copyright (c) 2016 STMicroelectronics.
  * All rights reserved.
  *
  * This software is licensed under terms that can be found in the LICENSE file
  * in the root directory of this software component.
  * If no LICENSE file comes with this software, it is provided AS-IS.
  *
  ******************************************************************************
  @verbatim
/* ==========================================================================
   TRIMMED QUOTE-ONLY COPY (ccid-firmware-rs, issue #135) — never compiled.
   Kept sections: the smartcard-mode "How to use" + Initialization docs,
   HAL_SMARTCARD_Init (GTPR prescaler/guard-time writes), the IRQ error-flag
   classification, and SMARTCARD_SetConfig (CR2 STOP + BRR sequence).
   Everything else (IO ops, DMA, callbacks) removed. Upstream pin: see
   reference/ST/PROVENANCE.md.
   ========================================================================== */


/** @defgroup SMARTCARD_Exported_Functions_Group1 SmartCard Initialization and de-initialization functions
  *  @brief    Initialization and Configuration functions
  *
@verbatim
  ==============================================================================
              ##### Initialization and Configuration functions #####
  ==============================================================================
  [..]
  This subsection provides a set of functions allowing to initialize the USART
  in Smartcard mode.
  [..]
  The Smartcard interface is designed to support asynchronous protocol Smartcards as
  defined in the ISO 7816-3 standard.
  [..]
  The USART can provide a clock to the smartcard through the SCLK output.
  In smartcard mode, SCLK is not associated to the communication but is simply derived
  from the internal peripheral input clock through a 5-bit prescaler.
  [..]
  (+) For the Smartcard mode only these parameters can be configured:
      (++) Baud Rate
      (++) Word Length => Should be 9 bits (8 bits + parity)
      (++) Stop Bit
      (++) Parity: => Should be enabled
      (++) USART polarity
      (++) USART phase
      (++) USART LastBit
      (++) Receiver/transmitter modes
      (++) Prescaler
      (++) GuardTime
      (++) NACKState: The Smartcard NACK state

     (+) Recommended SmartCard interface configuration to get the Answer to Reset from the Card:
        (++) Word Length = 9 Bits
        (++) 1.5 Stop Bit
        (++) Even parity
        (++) BaudRate = 12096 baud
        (++) Tx and Rx enabled
  [..]
  Please refer to the ISO 7816-3 specification for more details.

  [..]
   (@) It is also possible to choose 0.5 stop bit for receiving but it is recommended
       to use 1.5 stop bits for both transmitting and receiving to avoid switching
       between the two configurations.
  [..]
    The HAL_SMARTCARD_Init() function follows the USART  SmartCard configuration
    procedures (details for the procedures are available in reference manual
    (RM0430 for STM32F4X3xx MCUs and RM0402 for STM32F412xx MCUs
     RM0383 for STM32F411xC/E MCUs and RM0401 for STM32F410xx MCUs
     RM0090 for STM32F4X5xx/STM32F4X7xx/STM32F429xx/STM32F439xx MCUs
     RM0390 for STM32F446xx MCUs and RM0386 for STM32F469xx/STM32F479xx MCUs)).

@endverbatim

/* ... TRIMMED: HAL_SMARTCARD_Init body retained verbatim below ... */

  */

/**
  * @brief  Initializes the SmartCard mode according to the specified
  *         parameters in the SMARTCARD_InitTypeDef and create the associated handle.
  * @param  hsc    Pointer to a SMARTCARD_HandleTypeDef structure that contains
  *                the configuration information for SMARTCARD module.
  * @retval HAL status
  */
HAL_StatusTypeDef HAL_SMARTCARD_Init(SMARTCARD_HandleTypeDef *hsc)
{
  /* Check the SMARTCARD handle allocation */
  if(hsc == NULL)
  {
    return HAL_ERROR;
  }

  /* Check the parameters */
  assert_param(IS_SMARTCARD_INSTANCE(hsc->Instance));
  assert_param(IS_SMARTCARD_NACK_STATE(hsc->Init.NACKState));

  if(hsc->gState == HAL_SMARTCARD_STATE_RESET)
  {
    /* Allocate lock resource and initialize it */
    hsc->Lock = HAL_UNLOCKED;

#if USE_HAL_SMARTCARD_REGISTER_CALLBACKS == 1
    SMARTCARD_InitCallbacksToDefault(hsc);

    if (hsc->MspInitCallback == NULL)
    {
      hsc->MspInitCallback = HAL_SMARTCARD_MspInit;
    }

    /* Init the low level hardware */
    hsc->MspInitCallback(hsc);
#else
    /* Init the low level hardware : GPIO, CLOCK */
    HAL_SMARTCARD_MspInit(hsc);
#endif /* USE_HAL_SMARTCARD_REGISTER_CALLBACKS */
  }

  hsc->gState = HAL_SMARTCARD_STATE_BUSY;

  /* Set the Prescaler */
  MODIFY_REG(hsc->Instance->GTPR, USART_GTPR_PSC, hsc->Init.Prescaler);

  /* Set the Guard Time */
  MODIFY_REG(hsc->Instance->GTPR, USART_GTPR_GT, ((hsc->Init.GuardTime)<<8U));

  /* Set the Smartcard Communication parameters */
  SMARTCARD_SetConfig(hsc);

  /* In SmartCard mode, the following bits must be kept cleared:
  - LINEN bit in the USART_CR2 register
  - HDSEL and IREN bits in the USART_CR3 register.*/
  CLEAR_BIT(hsc->Instance->CR2, USART_CR2_LINEN);
  CLEAR_BIT(hsc->Instance->CR3, (USART_CR3_IREN | USART_CR3_HDSEL));

  /* Enable the SMARTCARD Parity Error Interrupt */
  SET_BIT(hsc->Instance->CR1, USART_CR1_PEIE);

  /* Enable the SMARTCARD Framing Error Interrupt */
  SET_BIT(hsc->Instance->CR3, USART_CR3_EIE);

  /* Enable the Peripheral */
  __HAL_SMARTCARD_ENABLE(hsc);

  /* Configure the Smartcard NACK state */
  MODIFY_REG(hsc->Instance->CR3, USART_CR3_NACK, hsc->Init.NACKState);

  /* Enable the SC mode by setting the SCEN bit in the CR3 register */
  hsc->Instance->CR3 |= (USART_CR3_SCEN);

  /* Initialize the SMARTCARD state*/
  hsc->ErrorCode = HAL_SMARTCARD_ERROR_NONE;
  hsc->gState= HAL_SMARTCARD_STATE_READY;
  hsc->RxState= HAL_SMARTCARD_STATE_READY;

  return HAL_OK;
}

/**
  * @brief DeInitializes the USART SmartCard peripheral
  * @param  hsc    Pointer to a SMARTCARD_HandleTypeDef structure that contains
  *                the configuration information for SMARTCARD module.

/* ... TRIMMED: DeInit, callbacks, IO operations, DMA ... */

#endif /* USE_HAL_SMARTCARD_REGISTER_CALLBACK */
  }

  return HAL_OK;
}

/**
  * @brief This function handles SMARTCARD interrupt request.
  * @param  hsc    Pointer to a SMARTCARD_HandleTypeDef structure that contains
  *                the configuration information for SMARTCARD module.
  * @retval None
  */
void HAL_SMARTCARD_IRQHandler(SMARTCARD_HandleTypeDef *hsc)
{
  uint32_t isrflags   = READ_REG(hsc->Instance->SR);
  uint32_t cr1its     = READ_REG(hsc->Instance->CR1);
  uint32_t cr3its     = READ_REG(hsc->Instance->CR3);
  uint32_t dmarequest = 0x00U;
  uint32_t errorflags = 0x00U;

  /* If no error occurs */
  errorflags = (isrflags & (uint32_t)(USART_SR_PE | USART_SR_FE | USART_SR_ORE | USART_SR_NE));
  if(errorflags == RESET)
  {
    /* SMARTCARD in mode Receiver -------------------------------------------------*/
    if(((isrflags & USART_SR_RXNE) != RESET) && ((cr1its & USART_CR1_RXNEIE) != RESET))
    {
      SMARTCARD_Receive_IT(hsc);
      return;
    }
  }

  /* If some errors occur */
  if((errorflags != RESET) && (((cr3its & USART_CR3_EIE) != RESET) || ((cr1its & (USART_CR1_RXNEIE | USART_CR1_PEIE)) != RESET)))
  {
    /* SMARTCARD parity error interrupt occurred ---------------------------*/
    if(((isrflags & SMARTCARD_FLAG_PE) != RESET) && ((cr1its & USART_CR1_PEIE) != RESET))
    {
      hsc->ErrorCode |= HAL_SMARTCARD_ERROR_PE;
    }

    /* SMARTCARD frame error interrupt occurred ----------------------------*/
    if(((isrflags & SMARTCARD_FLAG_FE) != RESET) && ((cr3its & USART_CR3_EIE) != RESET))
    {
      hsc->ErrorCode |= HAL_SMARTCARD_ERROR_FE;
    }

    /* SMARTCARD noise error interrupt occurred ----------------------------*/
    if(((isrflags & SMARTCARD_FLAG_NE) != RESET) && ((cr3its & USART_CR3_EIE) != RESET))
    {
      hsc->ErrorCode |= HAL_SMARTCARD_ERROR_NE;
    }

    /* SMARTCARD Over-Run interrupt occurred -------------------------------*/
    if(((isrflags & SMARTCARD_FLAG_ORE) != RESET) && (((cr1its & USART_CR1_RXNEIE) != RESET) || ((cr3its & USART_CR3_EIE) != RESET)))
    {
      hsc->ErrorCode |= HAL_SMARTCARD_ERROR_ORE;
    }
    /* Call the Error call Back in case of Errors --------------------------*/
    if(hsc->ErrorCode != HAL_SMARTCARD_ERROR_NONE)
    {

/* ... TRIMMED: transmit/receive IT+DMA bodies ... */


/**
  * @brief Configure the SMARTCARD peripheral
  * @param  hsc    Pointer to a SMARTCARD_HandleTypeDef structure that contains
  *                the configuration information for SMARTCARD module.
  * @retval None
  */
static void SMARTCARD_SetConfig(SMARTCARD_HandleTypeDef *hsc)
{
  uint32_t tmpreg = 0x00U;
  uint32_t pclk;

  /* Check the parameters */
  assert_param(IS_SMARTCARD_INSTANCE(hsc->Instance));
  assert_param(IS_SMARTCARD_POLARITY(hsc->Init.CLKPolarity));
  assert_param(IS_SMARTCARD_PHASE(hsc->Init.CLKPhase));
  assert_param(IS_SMARTCARD_LASTBIT(hsc->Init.CLKLastBit));
  assert_param(IS_SMARTCARD_BAUDRATE(hsc->Init.BaudRate));
  assert_param(IS_SMARTCARD_WORD_LENGTH(hsc->Init.WordLength));
  assert_param(IS_SMARTCARD_STOPBITS(hsc->Init.StopBits));
  assert_param(IS_SMARTCARD_PARITY(hsc->Init.Parity));
  assert_param(IS_SMARTCARD_MODE(hsc->Init.Mode));
  assert_param(IS_SMARTCARD_NACK_STATE(hsc->Init.NACKState));

  /* The LBCL, CPOL and CPHA bits have to be selected when both the transmitter and the
     receiver are disabled (TE=RE=0) to ensure that the clock pulses function correctly. */
  CLEAR_BIT(hsc->Instance->CR1, (USART_CR1_TE | USART_CR1_RE));

  /*---------------------------- USART CR2 Configuration ---------------------*/
  tmpreg = hsc->Instance->CR2;
  /* Clear CLKEN, CPOL, CPHA and LBCL bits */
  tmpreg &= (uint32_t)~((uint32_t)(USART_CR2_CPHA | USART_CR2_CPOL | USART_CR2_CLKEN | USART_CR2_LBCL));
  /* Configure the SMARTCARD Clock, CPOL, CPHA and LastBit -----------------------*/
  /* Set CPOL bit according to hsc->Init.CLKPolarity value */
  /* Set CPHA bit according to hsc->Init.CLKPhase value */
  /* Set LBCL bit according to hsc->Init.CLKLastBit value */
  /* Set Stop Bits: Set STOP[13:12] bits according to hsc->Init.StopBits value */
  tmpreg |= (uint32_t)(USART_CR2_CLKEN | hsc->Init.CLKPolarity |
                      hsc->Init.CLKPhase| hsc->Init.CLKLastBit | hsc->Init.StopBits);
  /* Write to USART CR2 */
  WRITE_REG(hsc->Instance->CR2, (uint32_t)tmpreg);

  tmpreg = hsc->Instance->CR2;

  /* Clear STOP[13:12] bits */
  tmpreg &= (uint32_t)~((uint32_t)USART_CR2_STOP);

  /* Set Stop Bits: Set STOP[13:12] bits according to hsc->Init.StopBits value */
  tmpreg |= (uint32_t)(hsc->Init.StopBits);

  /* Write to USART CR2 */
  WRITE_REG(hsc->Instance->CR2, (uint32_t)tmpreg);

  /*-------------------------- USART CR1 Configuration -----------------------*/
  tmpreg = hsc->Instance->CR1;

  /* Clear M, PCE, PS, TE and RE bits */
  tmpreg &= (uint32_t)~((uint32_t)(USART_CR1_M | USART_CR1_PCE | USART_CR1_PS | USART_CR1_TE | \
                                   USART_CR1_RE));

  /* Configure the SMARTCARD Word Length, Parity and mode:
     Set the M bits according to hsc->Init.WordLength value
     Set PCE and PS bits according to hsc->Init.Parity value
     Set TE and RE bits according to hsc->Init.Mode value */
  tmpreg |= (uint32_t)hsc->Init.WordLength | hsc->Init.Parity | hsc->Init.Mode;

  /* Write to USART CR1 */
  WRITE_REG(hsc->Instance->CR1, (uint32_t)tmpreg);

  /*-------------------------- USART CR3 Configuration -----------------------*/
  /* Clear CTSE and RTSE bits */
  CLEAR_BIT(hsc->Instance->CR3, (USART_CR3_RTSE | USART_CR3_CTSE));

  /*-------------------------- USART BRR Configuration -----------------------*/
#if defined(USART6)
  if((hsc->Instance == USART1) || (hsc->Instance == USART6))
  {
    pclk = HAL_RCC_GetPCLK2Freq();
    hsc->Instance->BRR = SMARTCARD_BRR(pclk, hsc->Init.BaudRate);
  }
#else
  if(hsc->Instance == USART1)
  {
    pclk = HAL_RCC_GetPCLK2Freq();
    hsc->Instance->BRR = SMARTCARD_BRR(pclk, hsc->Init.BaudRate);
  }
#endif /* USART6 */
  else
  {
    pclk = HAL_RCC_GetPCLK1Freq();
    hsc->Instance->BRR = SMARTCARD_BRR(pclk, hsc->Init.BaudRate);
  }
}

/**
  * @}
  */

#endif /* HAL_SMARTCARD_MODULE_ENABLED */
/**
  * @}
  */

/**
  * @}
  */


