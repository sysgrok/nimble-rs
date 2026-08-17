/* The whole device: no bootloader, no TF-M split (FLASH is RRAM here). */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 500K
  RAM : ORIGIN = 0x20000000, LENGTH = 96K
}
