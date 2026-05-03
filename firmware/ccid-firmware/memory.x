/* STM32F469 F:2048K R:256K */
MEMORY
{
  /* NOTE K = KiBi = 1024 bytes */
  /* Main SRAM is 256KB (0x20000000 - 0x2003FFFF) */
  /* CCM RAM (64KB at 0x10000000) is separate and not used */
  FLASH : ORIGIN = 0x08000000, LENGTH = 2048K
  RAM : ORIGIN = 0x20000000, LENGTH = 256K
}

/* This is where the call stack will be allocated. */
/* The stack is of the full descending type. */
/* NOTE Do NOT modify _stack_start unless you know what you are doing */
_stack_start = ORIGIN(RAM) + LENGTH(RAM);
