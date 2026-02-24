package proxy

import (
	"fmt"

	pb "ployz/internal/controlplane/pb"

	"google.golang.org/protobuf/encoding/protowire"
	"google.golang.org/protobuf/proto"
)

// One2ManyResponder converts upstream responses into messages from upstreams, so that multiple
// successful and failure responses might be returned in One2Many mode.
//
// Adapted from github.com/psviderski/uncloud/internal/machine/api/proxy.
type One2ManyResponder struct {
	machineAddr string
	machineID   string
}

// AppendInfo enhances upstream response with machine metadata.
//
// This method depends on grpc protobuf response structure. Each response should
// look like:
//
//	message SomeResponse {
//	  repeated SomeReply messages = 1; // field ID == 1
//	}
//
//	message SomeReply {
//	  Metadata metadata = 1;
//	  <other fields ...>
//	}
//
// The proxy injects Metadata into each SomeReply by appending serialized
// metadata bytes to the inner message and adjusting the outer length prefix.
func (b *One2ManyResponder) AppendInfo(streaming bool, resp []byte) ([]byte, error) {
	payload, err := proto.Marshal(&pb.Empty{
		Metadata: &pb.Metadata{
			MachineAddr: b.machineAddr,
			MachineId:   b.machineID,
		},
	})

	if streaming {
		return append(resp, payload...), err
	}

	const (
		metadataField = 1 // field number in proto definition for repeated response
		metadataType  = 2 // "string" for embedded messages
	)

	typ, n1 := protowire.ConsumeVarint(resp)
	if n1 < 0 {
		return nil, protowire.ParseError(n1)
	}

	_, n2 := protowire.ConsumeVarint(resp[n1:]) // length
	if n2 < 0 {
		return nil, protowire.ParseError(n2)
	}

	if typ != (metadataField<<3)|metadataType {
		return nil, fmt.Errorf("unexpected message format: %d", typ)
	}

	if n1+n2 > len(resp) {
		return nil, fmt.Errorf("unexpected message size: %d", len(resp))
	}

	// Cut off embedded message header and rebuild with new length.
	resp = resp[n1+n2:]
	prefix := protowire.AppendVarint(
		protowire.AppendVarint(nil, (metadataField<<3)|metadataType),
		uint64(len(resp)+len(payload)),
	)
	resp = append(prefix, resp...)

	return append(resp, payload...), err
}

// BuildError converts upstream error into a message with error metadata.
func (b *One2ManyResponder) BuildError(streaming bool, err error) ([]byte, error) {
	var resp proto.Message = &pb.Empty{
		Metadata: &pb.Metadata{
			MachineAddr: b.machineAddr,
			MachineId:   b.machineID,
			Error:       err.Error(),
		},
	}

	if !streaming {
		resp = &pb.EmptyResponse{
			Messages: []*pb.Empty{
				resp.(*pb.Empty),
			},
		}
	}

	return proto.Marshal(resp)
}
