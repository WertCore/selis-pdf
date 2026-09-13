# Selis error codes

Generated from `crates/selis-error/codes.toml`. Do not edit.

`doc_state` answers: *what happened to the user's document?*

| Code | Name | Kind | Doc state | Retryable | Meaning | User message |
|---|---|---|---|---|---|---|
| 1000 | `NOT_A_PDF` | Malformed | NotLoaded | no | The bytes do not begin with a PDF header and no header was found within the scan window. | This file does not look like a PDF. |
| 1001 | `LEX_UNEXPECTED_BYTE` | Malformed | NotLoaded | no | The lexer encountered a byte that cannot begin any COS token. | This PDF contains a syntax error and could not be read. |
| 1002 | `LEX_UNTERMINATED_STRING` | Malformed | NotLoaded | no | A literal or hexadecimal string ran to end of input without a terminator. | This PDF contains an unterminated text value. |
| 1003 | `LEX_NUMBER_OVERFLOW` | Malformed | NotLoaded | no | A numeric token exceeded the representable range. | This PDF contains a number that is out of range. |
| 1010 | `OBJ_UNEXPECTED` | Malformed | PartiallyLoaded | no | An object of an unexpected COS type was found where the structure requires another. | This PDF's internal structure is inconsistent. |
| 1011 | `OBJ_MISSING` | Malformed | PartiallyLoaded | no | An indirect reference resolved to no object in any revision. | This PDF refers to content that is missing. |
| 1012 | `OBJ_CYCLE` | Malformed | PartiallyLoaded | no | Reference resolution revisited an object already on the resolution path. | This PDF contains a circular reference. |
| 1013 | `OBJ_GENERATION_MISMATCH` | Malformed | PartiallyLoaded | no | The object header's generation number does not match the cross-reference entry. | This PDF's index disagrees with its content. |
| 1100 | `XREF_NOT_FOUND` | Malformed | NotLoaded | no | No startxref keyword was found in the tail scan window. | This PDF has no readable index. |
| 1101 | `XREF_MALFORMED` | Malformed | NotLoaded | no | A cross-reference table or stream was structurally invalid. | This PDF's index is damaged. |
| 1102 | `XREF_PREV_CYCLE` | Malformed | NotLoaded | no | The /Prev chain of cross-reference sections revisits an offset already seen. | This PDF's revision history contains a loop. |
| 1103 | `TRAILER_MISSING_ROOT` | Malformed | NotLoaded | no | No trailer in any revision supplies a usable /Root catalog reference. | This PDF does not declare its document root. |
| 1204 | `XREF_UNRECOVERABLE` | Malformed | NotLoaded | no | The cross-reference data is damaged beyond reconstruction. | This PDF's index is damaged and could not be rebuilt. |
| 1205 | `OBJSTM_MALFORMED` | Malformed | PartiallyLoaded | no | A compressed object stream (/ObjStm) had an invalid header, /N, or /First. | This PDF contains a damaged object container. |
| 1206 | `OBJSTM_NESTED` | Malformed | PartiallyLoaded | no | An object stream was found inside another object stream, which the format forbids. | This PDF nests object containers illegally. |
| 1300 | `PAGE_TREE_MALFORMED` | Malformed | PartiallyLoaded | no | The page tree is not a tree: a node was reachable by two paths, or a kid was not a node. | This PDF's page structure is damaged. |
| 1301 | `PAGE_OUT_OF_RANGE` | Invalid | Loaded | no | A page index was requested that the document does not contain. | That page does not exist in this document. |
| 1302 | `STREAM_LENGTH_MISMATCH` | Malformed | PartiallyLoaded | no | A stream's /Length disagreed with the position of its endstream keyword. | This PDF contains a stream with an incorrect declared length. |
| 1500 | `FILTER_UNKNOWN` | Unsupported | PartiallyLoaded | no | A stream declared a filter name the engine does not implement. | This PDF uses a compression method Selis does not yet support. |
| 1501 | `FILTER_PARAM_INVALID` | Malformed | PartiallyLoaded | no | A /DecodeParms entry was absent, of the wrong type, or out of range for its filter. | This PDF has invalid compression settings. |
| 1502 | `FILTER_CHAIN_TOO_LONG` | Malformed | PartiallyLoaded | no | A filter chain exceeded the configured maximum length. | This PDF applies an unreasonable number of compression layers. |
| 1510 | `FLATE_CORRUPT` | Malformed | PartiallyLoaded | no | A FlateDecode stream could not be inflated and yielded no usable prefix. | Compressed data in this PDF is damaged. |
| 1511 | `PREDICTOR_INVALID` | Malformed | PartiallyLoaded | no | A PNG or TIFF predictor row used an undefined filter type, or the row length was wrong. | Compressed data in this PDF is damaged. |
| 1520 | `LZW_CORRUPT` | Malformed | PartiallyLoaded | no | An LZWDecode stream referenced a code outside the current dictionary. | Compressed data in this PDF is damaged. |
| 1530 | `ASCII_CORRUPT` | Malformed | PartiallyLoaded | no | An ASCIIHexDecode or ASCII85Decode stream contained an invalid character or group. | Encoded data in this PDF is damaged. |
| 1540 | `RUNLENGTH_CORRUPT` | Malformed | PartiallyLoaded | no | A RunLengthDecode stream ended inside a run. | Encoded data in this PDF is damaged. |
| 1545 | `DCT_CORRUPT` | Malformed | PartiallyLoaded | no | A DCTDecode stream could not be decoded as JPEG. | Image data in this PDF is damaged. |
| 1546 | `FAX_CORRUPT` | Malformed | PartiallyLoaded | no | A CCITTFaxDecode stream could not be decoded. | Fax image data in this PDF is damaged. |
| 1547 | `JBIG2_CORRUPT` | Malformed | PartiallyLoaded | no | A JBIG2Decode stream could not be decoded. | JBIG2 image data in this PDF is damaged. |
| 1548 | `JBIG2_UNSUPPORTED` | Unsupported | PartiallyLoaded | no | A JBIG2 segment type (symbol dict, text region, refinement) is not yet supported. | Selis cannot yet read this JBIG2 encoding. |
| 1550 | `IMAGE_UNSUPPORTED` | Unsupported | Loaded | no | An image used a codec, colour space, or bit depth the engine does not yet decode. | Selis cannot yet read this image format. |
| 1551 | `IMAGE_MALFORMED` | Malformed | Unchanged | no | An image's headers were self-inconsistent or its sample data was truncated. | This image file is damaged. |
| 1560 | `FUNCTION_INPUT_COUNT` | Invalid | PartiallyLoaded | no | A PDF function was evaluated with the wrong number of input components. | A colour function in this PDF received the wrong inputs. |
| 1561 | `FUNCTION_BUDGET` | Budget | PartiallyLoaded | no | A PDF function exceeded its evaluation budget (a type-4 calculator loop or stack overflow). | A colour function in this PDF used too much computation. |
| 1800 | `ENCRYPTED` | Auth | NotLoaded | yes | The document is encrypted and no password was supplied. | This PDF is password-protected. |
| 1801 | `WRONG_PASSWORD` | Auth | NotLoaded | yes | Neither the user nor the owner password validated against the /Encrypt dictionary. | That password is not correct for this PDF. |
| 1802 | `ENCRYPT_UNSUPPORTED` | Unsupported | NotLoaded | no | The document uses a security handler or revision the engine does not implement. | This PDF uses a protection method Selis does not support. |
| 1803 | `ENCRYPT_MALFORMED` | Malformed | NotLoaded | no | The /Encrypt dictionary was missing required entries or had invalid key lengths. | This PDF's protection data is damaged. |
| 1804 | `PERMISSION_DENIED` | Policy | Unchanged | no | The document's permission bits forbid the requested operation and no override was given. | This PDF's permissions do not allow that. |
| 1805 | `PERMS_TAMPERED` | Malformed | NotLoaded | no | The /Perms entry did not decrypt to the expected permission values (revision 6). | This PDF's permission data has been altered. |
| 1806 | `ALREADY_ENCRYPTED` | Invalid | NotLoaded | no | The requested operation requires an unencrypted document, but the document is encrypted. | This PDF is already password-protected. Remove the password with `selis unlock` first. |
| 1807 | `RECIPIENT_NO_MATCH` | Auth | NotLoaded | yes | The supplied public-key credential did not open any recipient of the PKCS#7 /Encrypt dictionary (SL-1.ENC.03). | The supplied certificate or private key does not open this PDF. |
| 2000 | `SHAPE_FONT` | Malformed | Unchanged | no | A shaping run referenced a font program that could not be parsed. | This PDF's text cannot be shaped because its font data is damaged. |
| 2300 | `SMASK_MALFORMED` | Malformed | Unchanged | no | A soft-mask dictionary or its rendered group was structurally invalid. | This PDF's transparency mask data is damaged. |
| 2301 | `PATTERN_MALFORMED` | Malformed | Unchanged | no | A tiling pattern had a non-positive step, a singular matrix, or an invalid tile. | This PDF's tiling pattern data is damaged. |
| 2800 | `WRITE_FAILED` | Io | Unchanged | yes | The destination rejected a write or could not be committed. | Selis could not write the output file. |
| 2801 | `PREFIX_VIOLATION` | Internal | Unchanged | no | An incremental save produced output whose prefix is not byte-identical to the source (invariant I2). | Selis stopped a save that would have altered your original file. |
| 2802 | `VERIFY_FAILED` | Internal | Unchanged | no | Post-write verification found the output did not match the model it was written from. | The saved file did not verify, so Selis discarded it and kept your original. |
| 2803 | `MUTATION_INVALID` | Invalid | Unchanged | no | A mutation referenced an object, page, or field that does not exist in the current model. | That change cannot be applied to this document. |
| 2804 | `SOURCE_CHANGED` | Io | Unchanged | no | The source document changed underneath an open session; the journal is no longer valid against it. | This file changed on disk while it was open. |
| 2805 | `NOTHING_TO_WRITE` | Invalid | Unchanged | no | An incremental save was requested with an empty journal. | There are no changes to save. |
| 2806 | `MERGE_CONFLICT` | Invalid | Unchanged | no | Cross-document reconciliation found a collision the configured policy refuses to resolve. | These documents contain conflicting content that Selis will not silently merge. |
| 2807 | `SIGNATURE_WOULD_BREAK` | Policy | Unchanged | no | The requested operation rewrites bytes covered by an existing signature's /ByteRange. | This change would invalidate an existing digital signature. |
| 3600 | `CONFORMANCE_RULE_UNEVALUATED` | Unsupported | Loaded | no | A conformance rule could not be evaluated because the engine area it needs is below the required ladder level. | Selis cannot check that rule yet. |
| 4000 | `BUDGET_BYTES` | Budget | PartiallyLoaded | yes | The operation's allocation budget was exhausted. | This document needs more memory than Selis is allowed to use here. |
| 4001 | `BUDGET_WALL` | Budget | PartiallyLoaded | yes | The operation's wall-clock deadline expired. | This document is taking too long to process. |
| 4002 | `BUDGET_DEPTH` | Budget | PartiallyLoaded | no | The nesting-depth budget was exhausted. | This document nests content too deeply. |
| 4003 | `BUDGET_OBJECTS` | Budget | PartiallyLoaded | yes | The indirect-object resolution budget was exhausted. | This document contains more objects than Selis is allowed to load here. |
| 4004 | `BUDGET_PIXELS` | Budget | Loaded | yes | The rasterised-sample budget was exhausted. | This page is too large to render at this size. |
| 4010 | `BUDGET_POISONED` | Budget | PartiallyLoaded | no | A charge was attempted against a guard already poisoned by a prior exhaustion. | This operation was already stopped. |
| 4020 | `CANCELLED` | Cancelled | Unchanged | yes | The caller's cancel token was signalled. | Cancelled. |
| 4100 | `INTERNAL_PANIC` | Internal | Unchanged | no | A panic was caught at a binding boundary and converted to an error. Always a bug. | Selis hit an internal error. Your document was not modified. |
| 4101 | `INTERNAL_INVARIANT` | Internal | Unchanged | no | An engine invariant was violated. Always a bug. | Selis hit an internal error. Your document was not modified. |
| 4300 | `FEATURE_NOT_ENTITLED` | Policy | Unchanged | no | The active entitlement set does not include the requested feature. | That feature is not included in your plan. |
| 4301 | `ACTIVE_CONTENT_BLOCKED` | Policy | Loaded | no | The document requested JavaScript, a launch/URI action, or an external stream reference while active content was disabled. | Selis blocked this document from running embedded code. |
| 4302 | `CLOUD_CONSENT_REQUIRED` | Policy | Unchanged | no | An operation would send document-derived data off the device without a consent token. | This action would send your document off this device, so Selis stopped it. |
| 5000 | `IO_READ_FAILED` | Io | NotLoaded | yes | The source returned an error for a read within its declared length. | Selis could not read this file. |
| 5001 | `IO_UNEXPECTED_EOF` | Io | PartiallyLoaded | no | A read within the source's declared length returned fewer bytes than the source claims exist. | This file appears to be truncated. |
| 5002 | `IO_PENDING` | Io | PartiallyLoaded | yes | The requested range is not resident; the caller must supply it and re-drive the parse. | Waiting for more of this document to arrive. |
| 5003 | `IO_NOT_RANDOM_ACCESS` | Io | PartiallyLoaded | yes | An operation requiring seeking was attempted on a stream-only source. | This document must be fully downloaded before that action. |
| 5004 | `SINK_FINISHED` | Internal | Unchanged | no | A write was attempted against a sink that was already finished. | Selis hit an internal error. Your document was not modified. |
| 5005 | `SOURCE_TOO_LARGE` | Budget | NotLoaded | no | The source's declared length exceeds the addressable or budgeted maximum. | This file is too large for Selis to open here. |
| 6000 | `BINDING_BAD_HANDLE` | Invalid | Unchanged | no | A handle passed across a binding boundary was null, stale, or of the wrong type. | Selis hit an internal error. Your document was not modified. |
| 6001 | `BINDING_BAD_ARGUMENT` | Invalid | Unchanged | no | An argument across a binding boundary failed validation before entering the engine. | That request was not valid. |
| 6010 | `SANDBOX_MEMORY_CAP` | Budget | Loaded | yes | A sandboxed codec module tried to grow its linear memory or a table beyond its hard cap. | This content needs more memory than the sandboxed codec is allowed here. |
| 6011 | `SANDBOX_FUEL` | Budget | Loaded | yes | A sandboxed codec module exhausted its compute budget before finishing. | This content took too long to decode inside the sandbox. |
| 6012 | `SANDBOX_TRAP` | Internal | Loaded | no | A sandboxed codec module trapped during execution and was contained. | Selis stopped a decoding engine fault before it could do harm. |
| 6013 | `SANDBOX_IMPORT_DENIED` | Invalid | Loaded | no | A codec module declared host imports; the sandbox host defines none and refuses the module. | A decoding component tried to reach outside its sandbox and was stopped. |
| 6014 | `SANDBOX_PROTOCOL` | Invalid | Loaded | no | A codec module violated the copy-in/copy-out buffer protocol (bad export, pointer, or length). | A decoding component broke its sandbox contract and was stopped. |
| 6015 | `SANDBOX_MODULE_ERROR` | Malformed | Loaded | no | A sandboxed codec module reported its input as undecodable via the buffer-protocol status code. | This content could not be decoded. |
| 6016 | `SANDBOX_HOST_ERROR` | Internal | Loaded | no | The codec sandbox host itself failed to initialise or operate. | Selis hit an internal error in the codec sandbox. Your document was not modified. |
| 6017 | `BINDING_UNSUPPORTED_OP` | Unsupported | Unchanged | no | A request crossing a binding boundary named an operation, source adapter, or protocol version this engine build does not provide (a capability or version mismatch, not a malformed request). | That action is not available in this version of Selis. |
