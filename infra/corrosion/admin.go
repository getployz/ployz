package corrosion

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/netip"
	"time"
)

// maxFrameSize is the upper bound on a single Tokio length-delimited frame.
// Corrosion admin responses are small JSON payloads; 16 MiB is generous.
const maxFrameSize = 16 << 20

// AdminClient talks to Corrosion's admin Unix domain socket.
type AdminClient struct {
	sockPath string
}

// NewAdminClient creates a client for the Corrosion admin socket.
func NewAdminClient(sockPath string) *AdminClient {
	return &AdminClient{sockPath: sockPath}
}

// AdminResponse is a single response frame from the admin socket.
type AdminResponse struct {
	JSON map[string]any
	Err  error
}

// SendCommand sends a command and returns a channel of responses.
// The channel closes after the final or error response.
func (c *AdminClient) SendCommand(cmd []byte) (<-chan AdminResponse, error) {
	conn, err := net.Dial("unix", c.sockPath)
	if err != nil {
		return nil, fmt.Errorf("connect to admin socket: %w", err)
	}

	if _, err = conn.Write(encodeFrame(cmd)); err != nil {
		conn.Close()
		return nil, fmt.Errorf("send admin command: %w", err)
	}

	ch := make(chan AdminResponse)
	go func() {
		defer close(ch)
		defer conn.Close()

		for {
			data, err := readFrame(conn)
			if err != nil {
				ch <- AdminResponse{Err: err}
				return
			}

			var decoded any
			if err = json.Unmarshal(data, &decoded); err != nil {
				ch <- AdminResponse{Err: fmt.Errorf("unmarshal admin response: %w", err)}
				return
			}

			switch v := decoded.(type) {
			case string:
				if v == "Success" {
					return
				}
			case map[string]any:
				if errData, ok := v["Error"].(map[string]any); ok {
					if errMsg, ok := errData["msg"].(string); ok {
						ch <- AdminResponse{Err: errors.New(errMsg)}
					} else {
						ch <- AdminResponse{Err: fmt.Errorf("admin error: %v", errData)}
					}
					return
				}
				if jsonData, ok := v["Json"].(map[string]any); ok {
					ch <- AdminResponse{JSON: jsonData}
				}
			}
		}
	}()

	return ch, nil
}

// encodeFrame creates a length-delimited Tokio frame.
func encodeFrame(data []byte) []byte {
	frame := make([]byte, 4+len(data))
	binary.BigEndian.PutUint32(frame, uint32(len(data)))
	copy(frame[4:], data)
	return frame
}

// readFrame reads a length-delimited Tokio frame.
func readFrame(conn net.Conn) ([]byte, error) {
	var head [4]byte
	if _, err := io.ReadFull(conn, head[:]); err != nil {
		return nil, fmt.Errorf("read frame head: %w", err)
	}
	length := binary.BigEndian.Uint32(head[:])
	if length > maxFrameSize {
		return nil, fmt.Errorf("frame too large: %d bytes (max %d)", length, maxFrameSize)
	}
	data := make([]byte, length)
	if _, err := io.ReadFull(conn, data); err != nil {
		return nil, fmt.Errorf("read frame data: %w", err)
	}
	return data, nil
}

// MemberState describes the SWIM membership state of a cluster member.
type MemberState string

const (
	MemberAlive   MemberState = "Alive"
	MemberSuspect MemberState = "Suspect"
	MemberDown    MemberState = "Down"
)

// ClusterMember is the membership state of a single cluster node.
type ClusterMember struct {
	ID        string
	Addr      netip.AddrPort
	State     MemberState
	Timestamp time.Time
}

// ClusterMembers returns the current SWIM membership states.
// If latest is true, only the most recent state per member is returned.
func (c *AdminClient) ClusterMembers(latest bool) ([]ClusterMember, error) {
	respCh, err := c.SendCommand([]byte(`{"Cluster":"MembershipStates"}`))
	if err != nil {
		return nil, fmt.Errorf("cluster members: %w", err)
	}

	var (
		members    []ClusterMember
		latestByID map[string]ClusterMember
		parseErr   error
	)
	if latest {
		latestByID = make(map[string]ClusterMember)
	}

	for r := range respCh {
		if r.Err != nil {
			return nil, fmt.Errorf("cluster members: %w", r.Err)
		}

		m, err := parseClusterMember(r.JSON)
		if err != nil {
			parseErr = errors.Join(parseErr, err)
			continue
		}

		if latest {
			if existing, ok := latestByID[m.ID]; !ok || existing.Timestamp.Before(m.Timestamp) {
				latestByID[m.ID] = m
			}
		} else {
			members = append(members, m)
		}
	}

	if latest {
		members = make([]ClusterMember, 0, len(latestByID))
		for _, m := range latestByID {
			members = append(members, m)
		}
	}
	return members, parseErr
}

func parseClusterMember(data map[string]any) (ClusterMember, error) {
	var m ClusterMember

	idObj, ok := data["id"].(map[string]any)
	if !ok {
		return m, fmt.Errorf("parse cluster member: missing 'id' object")
	}

	id, ok := idObj["id"].(string)
	if !ok {
		return m, fmt.Errorf("parse cluster member: missing 'id.id'")
	}
	m.ID = id

	addr, ok := idObj["addr"].(string)
	if !ok {
		return m, fmt.Errorf("parse cluster member: missing 'id.addr'")
	}
	var err error
	m.Addr, err = netip.ParseAddrPort(addr)
	if err != nil {
		return m, fmt.Errorf("parse cluster member addr: %w", err)
	}

	stateStr, ok := data["state"].(string)
	if !ok {
		return m, fmt.Errorf("parse cluster member: missing 'state'")
	}
	switch MemberState(stateStr) {
	case MemberAlive, MemberSuspect, MemberDown:
		m.State = MemberState(stateStr)
	default:
		return m, fmt.Errorf("parse cluster member: unknown state %q", stateStr)
	}

	ts, ok := idObj["ts"].(float64)
	if !ok {
		return m, fmt.Errorf("parse cluster member: missing 'id.ts'")
	}
	m.Timestamp = ntp64ToTime(uint64(ts))

	return m, nil
}

// ntp64ToTime converts a 64-bit NTP timestamp (Unix epoch) to time.Time.
func ntp64ToTime(ntp uint64) time.Time {
	secs := uint32(ntp >> 32)
	frac := uint32(ntp)
	nsecs := (uint64(frac) * 1_000_000_000) >> 32
	return time.Unix(int64(secs), int64(nsecs))
}
