#import <AVFoundation/AVFoundation.h>
#import <AudioToolbox/AudioToolbox.h>
#import <Foundation/Foundation.h>
#include "bridge.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static _Thread_local char failure[1024];
const char *rm_demux_error(void) {return failure;}
static int fail(NSString *message) {snprintf(failure,sizeof(failure),"%s",message.UTF8String ?: "system demux failed");return -1;}
void rm_demux_free(void *data) {free(data);}
static int64_t nanos(CMTime time) {return CMTIME_IS_NUMERIC(time) ? CMTimeConvertScale(time,1000000000,kCMTimeRoundingMethod_RoundTowardZero).value : 0;}
static NSArray *bytes(const void *data,size_t size) {NSMutableArray *out=[NSMutableArray arrayWithCapacity:size];const uint8_t *p=data;for(size_t i=0;i<size;i++)[out addObject:@(p[i])];return out;}
static uint64_t samples(CMTime duration,uint32_t rate) {return CMTIME_IS_NUMERIC(duration) && duration.value>0 ? (uint64_t)CMTimeConvertScale(duration,rate,kCMTimeRoundingMethod_RoundHalfAwayFromZero).value : 0;}
@interface RmTrack : NSObject
@property AVAssetReaderTrackOutput *output;
@property AVAssetTrack *track;
@property uint32_t rate;
@property BOOL pcm;
@property CMSampleBufferRef sample;
@property CMItemCount position;
@property size_t offset;
@property uint64_t startTrim;
@property uint64_t endTrim;
@property BOOL first;
@property NSString *codec;
@property int64_t manualShift;
@property int64_t validFrames;
@property uint64_t primeFrames;
@property uint64_t tailFrames;
@property uint32_t packetFrames;
@end
@implementation RmTrack
-(void)dealloc {if(_sample)CFRelease(_sample);}
@end
@interface RmReader : NSObject
@property AVURLAsset *asset;
@property AVAssetReader *reader;
@property NSArray<RmTrack*> *tracks;
@end
@implementation RmReader
@end
static int fill(RmReader *reader,RmTrack *track);
static NSString *audio_name(const AudioStreamBasicDescription *f) {
 switch(f->mFormatID) {
 case kAudioFormatMPEG4AAC: case kAudioFormatMPEG4AAC_HE: case kAudioFormatMPEG4AAC_HE_V2:return @"aac";
 case kAudioFormatMPEGLayer3:return @"mp3";case kAudioFormatFLAC:return @"flac";
 case kAudioFormatAppleLossless:return @"alac";case kAudioFormatOpus:return @"opus";
 case 'vorb':return @"vorbis";case kAudioFormatAC3:return @"ac3";case kAudioFormatEnhancedAC3:return @"eac3";
 case kAudioFormatLinearPCM: {
  BOOL f32=(f->mFormatFlags&kAudioFormatFlagIsFloat)!=0;BOOL sign=(f->mFormatFlags&kAudioFormatFlagIsSignedInteger)!=0;BOOL be=(f->mFormatFlags&kAudioFormatFlagIsBigEndian)!=0;
  if(f->mFormatFlags&kAudioFormatFlagIsNonInterleaved)return @"unsupported_planar_pcm";
  return [NSString stringWithFormat:@"pcm_%c%u%@",f32?'f':sign?'s':'u',f->mBitsPerChannel,f->mBitsPerChannel==8?@"":be?@"be":@"le"];
 }
 default:return [NSString stringWithFormat:@"system_%08x",(unsigned)f->mFormatID];
 }
}
static uint32_t be32(const uint8_t *p) {return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3];}
static void mp3_gapless(NSString *path,RmTrack *track) {
 FILE *file=fopen(path.fileSystemRepresentation,"rb");if(!file)return;
 uint8_t data[65536];size_t size=fread(data,1,sizeof(data),file);fclose(file);size_t at=0;
 if(size>=10&&!memcmp(data,"ID3",3)) {
  if((data[6]|data[7]|data[8]|data[9])&128)return;
  at=10+((size_t)data[6]<<21)+((size_t)data[7]<<14)+((size_t)data[8]<<7)+data[9];
 }
 if(at>size||size-at<4)return;const uint8_t *frame=data+at;
 if(frame[0]!=255||(frame[1]&0xe6)!=0xe2)return;
 BOOL mpeg1=(frame[1]&0x18)==0x18,mono=(frame[3]&0xc0)==0xc0;
 at+=4+((frame[1]&1)?0:2)+(mpeg1?(mono?17:32):(mono?9:17));
 if(at>size||size-at<8||(memcmp(data+at,"Xing",4)&&memcmp(data+at,"Info",4)))return;
 uint32_t flags=be32(data+at+4);at+=8;uint32_t count=0;
 for(unsigned bit=0;bit<4;bit++)if(flags&(1u<<bit)) {size_t n=bit==2?100:4;if(n>size-at)return;if(bit==0)count=be32(data+at);at+=n;}
 if(size-at<24||(memcmp(data+at,"LAME",4)!=0&&memcmp(data+at,"Lavc",4)!=0&&memcmp(data+at,"Lavf",4)!=0))return;
 uint32_t delay=((uint32_t)data[at+21]<<4)|(data[at+22]>>4),padding=((data[at+22]&15)<<8)|data[at+23];
 int64_t total=(int64_t)count*(mpeg1?1152:576);if(!count||total<delay+padding)return;
 track.manualShift=delay+529;track.primeFrames=delay+529;track.tailFrames=padding>529?padding-529:0;track.validFrames=total-delay-padding;
}
static int start(RmReader *state,int64_t target) {
 [state.reader cancelReading];NSError *error=nil;
 state.reader=[[AVAssetReader alloc] initWithAsset:state.asset error:&error];
 if(!state.reader)return fail(error.description);
 if(target>0) {
  CMTime begin=CMTimeMake(target,1000000);
  for(RmTrack *track in state.tracks)if([track.track.mediaType isEqualToString:AVMediaTypeVideo]) {
   AVSampleCursor *cursor=[track.track makeSampleCursorWithPresentationTimeStamp:begin];
   if(!cursor){begin=kCMTimeZero;break;}
   unsigned steps=0;
   while(!cursor.currentSampleSyncInfo.sampleIsFullSync && [cursor stepInDecodeOrderByCount:-1])if(++steps>65536)return fail(@"excessive video preroll");
   if(CMTimeCompare(cursor.presentationTimeStamp,begin)<0)begin=cursor.presentationTimeStamp;
  }
  state.reader.timeRange=CMTimeRangeFromTimeToTime(begin,state.asset.duration);
 }
 for(RmTrack *track in state.tracks) {
  if(track.sample){CFRelease(track.sample);track.sample=NULL;}
  track.position=0;track.offset=0;track.startTrim=0;track.endTrim=0;track.first=YES;
  track.output=[[AVAssetReaderTrackOutput alloc] initWithTrack:track.track outputSettings:nil];
  track.output.alwaysCopiesSampleData=NO;
  if(![state.reader canAddOutput:track.output])return fail(@"unsupported compressed track output");
  [state.reader addOutput:track.output];
 }
 return [state.reader startReading]?0:fail(state.reader.error.description);
}
int rm_demux_open(const char *path,void **reader,char **metadata) { @autoreleasepool {
 *reader=NULL;*metadata=NULL;RmReader *state=[RmReader new];
 state.asset=[AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:@(path)] options:@{AVURLAssetPreferPreciseDurationAndTimingKey:@YES}];
 NSMutableArray *tracks=[NSMutableArray array],*descriptions=[NSMutableArray array];
 NSMutableDictionary *tags=[NSMutableDictionary dictionary];
 for(AVMetadataItem *item in state.asset.commonMetadata) {
  NSString *name=item.commonKey;if(item.stringValue && name && ![name isEqualToString:AVMetadataCommonKeyArtwork])tags[name]=item.stringValue;
 }
 for(AVAssetTrack *track in state.asset.tracks) {
  BOOL video=[track.mediaType isEqualToString:AVMediaTypeVideo],audio=[track.mediaType isEqualToString:AVMediaTypeAudio];
  if(!video&&!audio)continue;
  if(descriptions.count>=64)return fail(@"too many system media tracks");
  CMFormatDescriptionRef format=(__bridge CMFormatDescriptionRef)track.formatDescriptions.firstObject;if(!format)return fail(@"missing system track description");
  NSString *codec=@"unsupported_video";NSMutableDictionary *init=[NSMutableDictionary dictionary];uint32_t width=0,height=0,rate=0,channels=0;
  if(audio) {
   const AudioStreamBasicDescription *asbd=CMAudioFormatDescriptionGetStreamBasicDescription(format);if(!asbd)return fail(@"missing audio stream description");
   rate=(uint32_t)asbd->mSampleRate;channels=asbd->mChannelsPerFrame;codec=audio_name(asbd);
   size_t size=0;const void *cookie=CMAudioFormatDescriptionGetMagicCookie(format,&size);if(size>65536)return fail(@"oversize audio cookie");
   init[@"extra_data"]=bytes(cookie,size);init[@"system_cookie"]=@YES;init[@"container"]=@"avfoundation";
   init[@"format_id"]=@(asbd->mFormatID);init[@"format_flags"]=@(asbd->mFormatFlags);init[@"frames_per_packet"]=@(asbd->mFramesPerPacket);
  } else {
   CMVideoDimensions dimensions=CMVideoFormatDescriptionGetDimensions(format);width=dimensions.width;height=dimensions.height;
   FourCharCode type=CMFormatDescriptionGetMediaSubType(format);codec=type==kCMVideoCodecType_H264?@"h264":type==kCMVideoCodecType_HEVC?@"hevc":@"unsupported_video";
   NSDictionary *atoms=(__bridge NSDictionary*)CMFormatDescriptionGetExtension(format,kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
   NSData *data=atoms[[codec isEqualToString:@"hevc"]?@"hvcC":@"avcC"];
   init[@"extra_data"]=data?@[bytes(data.bytes,data.length)]:@[];
  }
  NSDictionary *description=@{@"index":@(track.trackID),@"kind":audio?@"audio":@"video",@"codec":codec,@"time_base":@{@"num":@1,@"den":@(audio?rate:1000000000)},@"width":@(width),@"height":@(height),@"sample_rate":@(rate),@"channels":@(channels),@"attached_picture":@NO,@"metadata":@{},@"initialization":init};
  [descriptions addObject:description];RmTrack *item=[RmTrack new];item.track=track;item.rate=rate;item.codec=codec;item.packetFrames=audio?CMAudioFormatDescriptionGetStreamBasicDescription(format)->mFramesPerPacket:0;item.pcm=[codec hasPrefix:@"pcm_"];[tracks addObject:item];
 }
 if(!tracks.count)return fail(@"no system-readable audio/video tracks");state.tracks=tracks;
 if((tracks.count==1 && ((RmTrack*)tracks[0]).rate) || [state.asset.URL.pathExtension.lowercaseString isEqualToString:@"mp4"] || [state.asset.URL.pathExtension.lowercaseString isEqualToString:@"m4a"]) {
  AudioFileID file=NULL;
  if(!AudioFileOpenURL((__bridge CFURLRef)state.asset.URL,kAudioFileReadPermission,0,&file)) {
   AudioFilePacketTableInfo table={0};UInt32 size=sizeof(table);
   if(!AudioFileGetProperty(file,kAudioFilePropertyPacketTableInfo,&size,&table) && table.mNumberValidFrames>0) {
    RmTrack *track=nil;for(RmTrack *candidate in tracks)if(candidate.rate){track=candidate;break;}if(track){track.validFrames=table.mNumberValidFrames;track.primeFrames=MAX(0,table.mPrimingFrames);track.tailFrames=MAX(0,table.mRemainderFrames);}
   }
   AudioFileClose(file);
  }
 }

 for(RmTrack *track in tracks)if([track.codec isEqualToString:@"mp3"])mp3_gapless(state.asset.URL.path,track);
 CMTime duration=state.asset.duration;id length=CMTIME_IS_NUMERIC(duration)?@(CMTimeConvertScale(duration,1000000,kCMTimeRoundingMethod_RoundTowardZero).value):(id)[NSNull null];
 if(tracks.count==1 && ((RmTrack*)tracks[0]).validFrames>0)length=@( ((RmTrack*)tracks[0]).validFrames*1000000/((RmTrack*)tracks[0]).rate );
 NSDictionary *info=@{@"container":@"avfoundation",@"duration_us":length,@"origin_us":@0,@"metadata":tags,@"tracks":descriptions};
 if(start(state,0))return -1;
 for(NSUInteger i=0;i<tracks.count;i++) {
  RmTrack *track=tracks[i];if(fill(state,track)<0)return -1;
  NSMutableDictionary *init=descriptions[i][@"initialization"];
  if(track.rate) {
   init[@"timestamp_shift_samples"]=@0;
   init[@"num_frames"]=@(track.validFrames);init[@"delay_samples"]=@(track.primeFrames);init[@"padding_samples"]=@(track.tailFrames);
   // OpusHead pre-skip is applied once by the independent decoder.
   if([track.codec isEqualToString:@"opus"])track.startTrim=0;
  }
 }
 NSData *json=[NSJSONSerialization dataWithJSONObject:info options:0 error:nil];
 if(!json||json.length>1024*1024)return fail(@"oversize system metadata");
 char *text=malloc(json.length+1);if(!text)return fail(@"metadata allocation failed");memcpy(text,json.bytes,json.length);text[json.length]=0;
 *metadata=text;*reader=(__bridge_retained void*)state;return 0;
}}
static int fill(RmReader *reader,RmTrack *track) {
 while(!track.sample||track.position>=CMSampleBufferGetNumSamples(track.sample)) {
  if(track.sample){CFRelease(track.sample);track.sample=NULL;}
  track.sample=[track.output copyNextSampleBuffer];track.position=0;track.offset=0;
  if(!track.sample)return reader.reader.status==AVAssetReaderStatusFailed?fail(reader.reader.error.description):0;
  if(track.first&&track.rate) {
   CFTypeRef trim=CMGetAttachment(track.sample,kCMSampleBufferAttachmentKey_TrimDurationAtStart,NULL);
   if(trim)track.startTrim=samples(CMTimeMakeFromDictionary(trim),track.rate);
   if([track.codec isEqualToString:@"mp3"] && track.manualShift && nanos(CMSampleBufferGetPresentationTimeStamp(track.sample))==0)track.startTrim=track.primeFrames;
   track.first=NO;
  }
  CFTypeRef trim=CMGetAttachment(track.sample,kCMSampleBufferAttachmentKey_TrimDurationAtEnd,NULL);
  track.endTrim=trim&&track.rate?samples(CMTimeMakeFromDictionary(trim),track.rate):0;
 }
 return 1;
}
int rm_demux_read(void *handle,size_t limit,RmPacket *out) { @autoreleasepool {
 memset(out,0,sizeof(*out));RmReader *reader=(__bridge RmReader*)handle;RmTrack *selected=nil;int64_t earliest=INT64_MAX;
 for(RmTrack *track in reader.tracks) {
  int result=fill(reader,track);if(result<0)return result;if(!result)continue;
  CMSampleTimingInfo timing;if(CMSampleBufferGetSampleTimingInfo(track.sample,track.position,&timing))return fail(@"invalid compressed sample timing");
  int64_t time=nanos(CMTIME_IS_NUMERIC(timing.decodeTimeStamp)?timing.decodeTimeStamp:timing.presentationTimeStamp);
  if(!selected||time<earliest){selected=track;earliest=time;}
 }
 if(!selected)return 0;
 RmTrack *track=selected;CMSampleBufferRef sample=track.sample;CMItemCount count=track.pcm?MIN(4096,CMSampleBufferGetNumSamples(sample)-track.position):1;
 size_t size=0;for(CMItemCount i=0;i<count;i++){size_t n=CMSampleBufferGetSampleSize(sample,track.position+i);if(n>limit-size)return fail(@"compressed packet exceeds budget");size+=n;}
 if(!size)return fail(@"empty compressed sample");
 out->data=malloc(size);if(!out->data)return fail(@"packet allocation failed");out->size=size;
 OSStatus status=CMBlockBufferCopyDataBytes(CMSampleBufferGetDataBuffer(sample),track.offset,size,out->data);
 if(status){free(out->data);out->data=NULL;return fail(@"compressed data copy failed");}
 CMSampleTimingInfo timing;CMSampleBufferGetSampleTimingInfo(sample,track.position,&timing);
 out->stream=track.track.trackID;out->time_scale=track.rate?track.rate:1000000000;
 CMTime correction=CMTimeSubtract(CMSampleBufferGetOutputPresentationTimeStamp(sample),CMSampleBufferGetPresentationTimeStamp(sample));
 CFTypeRef startTrim=CMGetAttachment(sample,kCMSampleBufferAttachmentKey_TrimDurationAtStart,NULL);
 if(startTrim)correction=CMTimeSubtract(correction,CMTimeMakeFromDictionary(startTrim));
 CMTime pts=CMTimeAdd(timing.presentationTimeStamp,correction);
 CMTime dts=CMTimeAdd(CMTIME_IS_NUMERIC(timing.decodeTimeStamp)?timing.decodeTimeStamp:timing.presentationTimeStamp,correction);
 out->pts=CMTimeConvertScale(pts,out->time_scale,kCMTimeRoundingMethod_RoundHalfAwayFromZero).value-track.manualShift;
 out->dts=CMTimeConvertScale(dts,out->time_scale,kCMTimeRoundingMethod_RoundHalfAwayFromZero).value-track.manualShift;
 out->duration=CMTimeConvertScale(CMTimeMultiply(timing.duration,(int32_t)count),out->time_scale,kCMTimeRoundingMethod_RoundHalfAwayFromZero).value;
 if([track.codec isEqualToString:@"vorbis"] && track.startTrim)out->duration=0;
 out->trim_start=[track.codec isEqualToString:@"opus"]?0:track.startTrim;track.startTrim=0;
 out->trim_end=track.position+count==CMSampleBufferGetNumSamples(sample)?track.endTrim:0;
 if([track.codec isEqualToString:@"aac"] && track.packetFrames>0 && out->duration>0 && out->duration<track.packetFrames)out->trim_end=track.packetFrames-out->duration;
 if([track.codec isEqualToString:@"flac"]||[track.codec isEqualToString:@"alac"])out->trim_end=0;
 if(track.validFrames>0 && track.tailFrames && !track.pcm && ![track.codec isEqualToString:@"flac"] && ![track.codec isEqualToString:@"alac"] && out->pts+out->duration>track.validFrames)out->trim_end=MIN((uint64_t)(out->pts+out->duration-track.validFrames),(uint64_t)out->duration);
 CFArrayRef attachments=CMSampleBufferGetSampleAttachmentsArray(sample,false);out->keyframe=1;
 if(attachments && track.position<CFArrayGetCount(attachments)) {
  CFDictionaryRef attributes=CFArrayGetValueAtIndex(attachments,track.position);
  out->keyframe=CFDictionaryGetValue(attributes,kCMSampleAttachmentKey_NotSync)!=kCFBooleanTrue;
 }
 track.offset+=size;track.position+=count;return 1;
}}
int rm_demux_seek(void *handle,int64_t target) { @autoreleasepool {return start((__bridge RmReader*)handle,target);}}
int rm_demux_artwork(void *handle,size_t limit,uint8_t **data,size_t *size) { @autoreleasepool {
 *data=NULL;*size=0;RmReader *reader=(__bridge RmReader*)handle;
 for(AVMetadataItem *item in reader.asset.commonMetadata)if([item.commonKey isEqualToString:AVMetadataCommonKeyArtwork]) {
  NSData *image=item.dataValue;if(!image)return 0;if(image.length>limit)return fail(@"artwork exceeds budget");
  *data=malloc(image.length);if(!*data)return fail(@"artwork allocation failed");*size=image.length;memcpy(*data,image.bytes,image.length);return 1;
 }
 return 0;
}}
void rm_demux_close(void *handle) { @autoreleasepool {RmReader *reader=CFBridgingRelease(handle);[reader.reader cancelReading];}}
