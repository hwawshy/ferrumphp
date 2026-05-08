#include "ferrumphp.h"

void ferrumphp_error(int type, const char *format, ...) {
  va_list args;
  va_start(args, format);
  php_error(type, format, args);
  //vprintf(format, args);
  va_end(args);
}