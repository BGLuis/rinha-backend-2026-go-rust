package engine

//go:noescape
func SearchVectorFast(q *int16, scratch *byte) int32

//go:noescape
func Pause()
