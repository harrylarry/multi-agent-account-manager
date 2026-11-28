#include <stddef.h>
#include <stdint.h>

typedef struct DiehaoWire DiehaoWire;
typedef struct DiehaoWireBlob DiehaoWireBlob;

DiehaoWire *diehao_wire_new(void);
void diehao_wire_free(DiehaoWire *wire);
DiehaoWireBlob *diehao_wire_message_one(DiehaoWire *wire);
DiehaoWireBlob *diehao_wire_message_three(DiehaoWire *wire, const uint8_t *bytes, size_t length);
DiehaoWireBlob *diehao_wire_request(DiehaoWire *wire, const uint8_t *bytes, size_t length);
DiehaoWireBlob *diehao_wire_approval_decision(DiehaoWire *wire, const uint8_t *route, size_t route_length,
                                           const uint8_t *body, size_t body_length);
DiehaoWireBlob *diehao_wire_receive(DiehaoWire *wire, const uint8_t *bytes, size_t length);
const uint8_t *diehao_wire_blob_read(const DiehaoWireBlob *blob, size_t *length);
void diehao_wire_blob_free(DiehaoWireBlob *blob);
